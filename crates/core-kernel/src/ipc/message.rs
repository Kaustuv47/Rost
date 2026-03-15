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
