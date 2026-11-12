use crate::project::model::Project;

pub struct History {
    undo: Vec<Project>,
    redo: Vec<Project>,
    limit: usize,
}

impl Default for History {
    fn default() -> Self {
        History::new(50)
    }
}

impl History {
    pub fn new(limit: usize) -> History {
        History {
            undo: Vec::new(),
            redo: Vec::new(),
            limit: limit.max(1),
        }
    }

    pub fn record(&mut self, before: &Project) {
        self.undo.push(before.clone());
        if self.undo.len() > self.limit {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self, current: &mut Project) -> bool {
        if let Some(previous) = self.undo.pop() {
            self.redo.push(current.clone());
            *current = previous;
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self, current: &mut Project) -> bool {
        if let Some(next) = self.redo.pop() {
            self.undo.push(current.clone());
            *current = next;
            true
        } else {
            false
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn depth(&self) -> (usize, usize) {
        (self.undo.len(), self.redo.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_redo_roundtrip() {
        let mut history = History::new(10);
        let mut project = Project::new("a");
        let before = project.clone();
        project.name = "b".into();
        history.record(&before);
        assert!(history.undo(&mut project));
        assert_eq!(project.name, "a");
        assert!(history.redo(&mut project));
        assert_eq!(project.name, "b");
    }
}
