use crate::process::ProcessId;

/// Message structure for IPC
#[repr(C)]
pub struct Message {
    pub sender: ProcessId,
    pub data: [u64; 8], // 64 bytes of data
}

impl Message {
    /// Create a new message
    pub fn new(sender: ProcessId) -> Self {
        Message {
            sender,
            data: [0; 8],
        }
    }

    /// Set message data
    pub fn set_data(&mut self, offset: usize, value: u64) {
        if offset < 8 {
            self.data[offset] = value;
        }
    }

    /// Get message data
    pub fn get_data(&self, offset: usize) -> u64 {
        if offset < 8 {
            self.data[offset]
        } else {
            0
        }
    }
}

/// Message queue for a process
pub struct MessageQueue {
    messages: [Option<Message>; 16],
    head: usize,
    tail: usize,
    count: usize,
}

impl MessageQueue {
    /// Create a new message queue
    pub fn new() -> Self {
        MessageQueue {
            messages: [
                None, None, None, None, None, None, None, None,
                None, None, None, None, None, None, None, None,
            ],
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    /// Send a message to the queue
    pub fn send(&mut self, message: Message) -> bool {
        if self.count >= 16 {
            return false; // Queue full
        }

        self.messages[self.tail] = Some(message);
        self.tail = (self.tail + 1) % 16;
        self.count += 1;
        true
    }

    /// Receive a message from the queue
    pub fn receive(&mut self) -> Option<Message> {
        if self.count == 0 {
            return None;
        }

        let msg = self.messages[self.head].take();
        self.head = (self.head + 1) % 16;
        self.count -= 1;
        msg
    }

    /// Check if queue is empty
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}
