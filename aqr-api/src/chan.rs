//! Channels between tasks (`aqr_chan`, `aqr_send`, `aqr_recv`, `aqr_close`): of byte messages,
//! holding as many as they were made for, a sender waiting while one is full and a receiver
//! while one is empty.
//!
//! The ends are numbers, which tasks find in the copy of the working memory they start with:
//! [`channel`] makes them before the tasks that use them are spawned. Each end is the first
//! instance's to use it (the module's or a task's), and another using it fails the application,
//! so that what an instance receives does not depend on when the others run. A channel is
//! closed by its sender ([`Sender::close`]), or as the task that sent to it ends: a task that
//! may send nothing closes it itself, else its receiver waits for ever (a deadlock, which fails
//! the application). Dropping an end does nothing, as the spawner keeps its copy of the ends it
//! gave its tasks.
//!
//! Natively ([`emu`](crate::emu)) the channels are queues between the threads the tasks run on.

use alloc::vec::Vec;

/// The sending end of a channel.
#[derive(Clone, Copy, Debug)]
pub struct Sender(u32);

/// The receiving end of a channel.
#[derive(Clone, Copy, Debug)]
pub struct Receiver(u32);

/// A channel holding up to `cap` messages (at least one).
pub fn channel(cap: usize) -> (Sender, Receiver) {
    let c = crate::sys::chan(u32::try_from(cap.max(1)).unwrap_or(u32::MAX >> 1));
    (Sender(c), Receiver(c))
}

impl Sender {
    /// Sends `msg`, once there is room for it.
    pub fn send(&self, msg: &[u8]) {
        crate::sys::send(self.0, msg)
    }

    /// Closes the channel: its receiver gets `None` once it has every message.
    pub fn close(&self) {
        crate::sys::close(self.0)
    }
}

impl Receiver {
    /// The next message, once there is one; `None` once the channel is closed and empty.
    pub fn recv(&self) -> Option<Vec<u8>> {
        let mut buf = Vec::new();
        loop {
            let n = crate::sys::recv(self.0, &mut buf);
            if n < 0 {
                return None;
            }
            // (left queued where longer than the buffer)
            let n = n as usize;
            if n <= buf.len() {
                buf.truncate(n);
                return Some(buf);
            }
            buf.resize(n, 0);
        }
    }
}
