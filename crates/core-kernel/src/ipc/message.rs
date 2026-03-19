use crate::process::ProcessId;

const QUEUE_CAPACITY: usize = 16;
const DATA_FIELDS:    usize = 8;

/// A typed IPC message.
///
/// `sender` is **always overwritten by the kernel** at the syscall boundary
/// so it cannot be forged by user-space code.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct Message {
    /// Actual sender PID — stamped by the kernel, not trusted from user space.
    pub sender: ProcessId,
    /// Application-defined payload (8 × u64 = 64 bytes).
    pub data:   [u64; DATA_FIELDS],
}

impl Message {
    pub fn new(sender: ProcessId) -> Self {
        Message { sender, data: [0; DATA_FIELDS] }
    }

    pub fn set_data(&mut self, offset: usize, value: u64) {
        if offset < DATA_FIELDS { self.data[offset] = value; }
    }

    pub fn get_data(&self, offset: usize) -> u64 {
        if offset < DATA_FIELDS { self.data[offset] } else { 0 }
    }
}

/// Lightweight one-bit notification (no payload, seL4 Notification / QNX pulse).
///
/// Delivered by ORing bits into `pending_notification`; consumed atomically.
/// Cheaper than a full `Message` for event signalling.
#[derive(Copy, Clone, Debug)]
pub struct Notification {
    pub sender: ProcessId,
    /// Bitmask of events being signalled.
    pub word:   u64,
}

/// Fixed-capacity FIFO message queue (circular buffer, capacity 16).
pub struct MessageQueue {
    messages:             [Option<Message>; QUEUE_CAPACITY],
    head:                 usize,
    tail:                 usize,
    count:                usize,
    /// Pending notification word — bits ORed together on each `notify()`.
    pub pending_notification: u64,
}

impl MessageQueue {
    pub fn new() -> Self {
        MessageQueue {
            messages: [
                None, None, None, None, None, None, None, None,
                None, None, None, None, None, None, None, None,
            ],
            head:                 0,
            tail:                 0,
            count:                0,
            pending_notification: 0,
        }
    }

    pub fn send(&mut self, message: Message) -> bool {
        if self.count >= QUEUE_CAPACITY { return false; }
        self.messages[self.tail] = Some(message);
        self.tail = (self.tail + 1) % QUEUE_CAPACITY;
        self.count += 1;
        true
    }

    pub fn receive(&mut self) -> Option<Message> {
        if self.count == 0 { return None; }
        let msg = self.messages[self.head].take();
        self.head = (self.head + 1) % QUEUE_CAPACITY;
        self.count -= 1;
        msg
    }

    /// Post a notification word (ORed into `pending_notification`).
    pub fn notify(&mut self, word: u64) {
        self.pending_notification |= word;
    }

    /// Consume the pending notification word, returning it and clearing it.
    pub fn poll_notification(&mut self) -> Option<u64> {
        if self.pending_notification == 0 { return None; }
        let w = self.pending_notification;
        self.pending_notification = 0;
        Some(w)
    }

    pub fn is_empty(&self) -> bool { self.count == 0 }
    pub fn is_full(&self)  -> bool { self.count >= QUEUE_CAPACITY }
    pub fn len(&self)      -> usize { self.count }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(n: u32) -> ProcessId { ProcessId::new(n) }

    fn msg(sender: u32, tag: u64) -> Message {
        let mut m = Message::new(pid(sender));
        m.data[0] = tag;
        m
    }

    /// Empty queue must report is_empty and receive must return None.
    #[test]
    fn test_empty_queue() {
        let mut q = MessageQueue::new();
        assert!(q.is_empty());
        assert!(!q.is_full());
        assert_eq!(q.len(), 0);
        assert!(q.receive().is_none());
    }

    /// Messages are dequeued in FIFO order.
    #[test]
    fn test_fifo_order() {
        let mut q = MessageQueue::new();
        assert!(q.send(msg(1, 100)));
        assert!(q.send(msg(2, 200)));
        assert!(q.send(msg(3, 300)));

        assert_eq!(q.receive().unwrap().data[0], 100);
        assert_eq!(q.receive().unwrap().data[0], 200);
        assert_eq!(q.receive().unwrap().data[0], 300);
        assert!(q.is_empty());
    }

    /// Queue refuses a 17th message when at capacity (capacity = 16).
    #[test]
    fn test_full_queue_rejects_send() {
        let mut q = MessageQueue::new();
        for i in 0..16 {
            assert!(q.send(msg(1, i as u64)), "slot {i} should succeed");
        }
        assert!(q.is_full());
        assert!(!q.send(msg(1, 99)), "17th send must be rejected");
    }

    /// After filling and draining, the circular buffer wraps correctly.
    #[test]
    fn test_wrap_around() {
        let mut q = MessageQueue::new();
        // Fill to capacity.
        for i in 0..16 { q.send(msg(1, i as u64)); }
        // Drain 8 from the head.
        for _ in 0..8 { q.receive(); }
        // Append 8 more — these wrap the tail pointer around.
        for i in 16..24 { q.send(msg(2, i as u64)); }
        assert_eq!(q.len(), 16);
        // Verify remaining order: indices 8..24.
        for i in 8..24u64 {
            let got = q.receive().unwrap().data[0];
            assert_eq!(got, i, "expected tag {i}, got {got}");
        }
        assert!(q.is_empty());
    }

    /// Notification word is ORed in; poll_notification consumes it atomically.
    #[test]
    fn test_notification_or_and_consume() {
        let mut q = MessageQueue::new();
        q.notify(0b0001);
        q.notify(0b0110);
        assert_eq!(q.poll_notification(), Some(0b0111));
        // Second poll must return None (word is cleared).
        assert_eq!(q.poll_notification(), None);
    }

    /// poll_notification on an untouched queue returns None.
    #[test]
    fn test_notification_empty() {
        let mut q = MessageQueue::new();
        assert_eq!(q.poll_notification(), None);
    }
}
