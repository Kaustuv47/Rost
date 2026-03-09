use crate::process::ProcessId;

const QUEUE_CAPACITY: usize = 16;
const DATA_FIELDS: usize = 8;

#[repr(C)]
pub struct Message {
    pub sender: ProcessId,
    pub data: [u64; DATA_FIELDS],
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

pub struct MessageQueue {
    messages: [Option<Message>; QUEUE_CAPACITY],
    head: usize,
    tail: usize,
    count: usize,
}

impl MessageQueue {
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

    pub fn is_empty(&self) -> bool { self.count == 0 }
}
