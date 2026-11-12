//! Messages the dashboard holds while a turn runs. Pure.

#[derive(Debug, Clone)]
pub struct Held {
    pub id: String,
    pub cid: String,
    pub text: String,
    /// Leaves on its own when a turn ends. Cleared by Stop: after a stop
    /// nothing runs that the owner did not confirm.
    pub auto: bool,
}

#[derive(Default)]
pub struct Queue {
    items: Vec<Held>,
    next: u64,
}

impl Queue {
    pub fn hold(&mut self, cid: String, text: String) -> String {
        self.next += 1;
        let id = format!("h{}", self.next);
        self.items.push(Held { id: id.clone(), cid, text, auto: true });
        id
    }

    pub fn take(&mut self, id: &str) -> Option<Held> {
        let i = self.items.iter().position(|h| h.id == id)?;
        Some(self.items.remove(i))
    }

    /// The head, only when it may leave on its own; a newer message never
    /// jumps an older one that was stopped.
    pub fn next_auto(&mut self) -> Option<Held> {
        if self.items.first()?.auto {
            Some(self.items.remove(0))
        } else {
            None
        }
    }

    pub fn stop_all(&mut self) {
        self.items.iter_mut().for_each(|h| h.auto = false);
    }

    pub fn list(&self) -> &[Held] {
        &self.items
    }

    pub fn clear(&mut self) -> Vec<Held> {
        std::mem::take(&mut self.items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_messages_leave_in_order_and_only_while_nothing_was_stopped() {
        let mut q = Queue::default();
        let (a, b) = (q.hold("c1".into(), "one".into()), q.hold("c2".into(), "two".into()));
        assert_eq!((a.as_str(), b.as_str()), ("h1", "h2"));
        assert_eq!(q.next_auto().unwrap().text, "one");
        q.stop_all();
        assert!(q.next_auto().is_none(), "after Stop nothing leaves on its own");
        let c = q.hold("c3".into(), "three".into());
        assert!(q.next_auto().is_none(), "a newer message does not jump an older held one");
        assert_eq!(q.take(&b).unwrap().cid, "c2");
        assert_eq!(q.next_auto().unwrap().text, "three");
        assert!(q.take(&c).is_none() && q.take("h9").is_none());
        q.hold("c4".into(), "four".into());
        assert_eq!(q.clear().len(), 1);
        assert!(q.list().is_empty());
    }
}
