use crate::point::{FaultAction, FaultPoint};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy)]
pub struct Rule {
    pub point: FaultPoint,
    pub trigger_count: u64,
    pub action: FaultAction,
}

#[derive(Default)]
pub struct FaultPlan {
    rules: Vec<Rule>,
    visits: HashMap<FaultPoint, u64>,
}
impl FaultPlan {
    #[must_use]
    pub fn new(rules: Vec<Rule>) -> Self {
        Self {
            rules,
            visits: HashMap::new(),
        }
    }
    pub fn visit(&mut self, point: FaultPoint) -> Option<FaultAction> {
        let count = self.visits.entry(point).or_default();
        *count += 1;
        self.rules
            .iter()
            .find(|rule| rule.point == point && rule.trigger_count == *count)
            .map(|rule| rule.action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nth_trigger_is_exact_and_replayable() {
        let rule = Rule {
            point: FaultPoint::UpdateAfterCommitUpload,
            trigger_count: 2,
            action: FaultAction::DropConnection,
        };
        for _ in 0..2 {
            let mut plan = FaultPlan::new(vec![rule]);
            assert_eq!(plan.visit(rule.point), None);
            assert_eq!(plan.visit(rule.point), Some(FaultAction::DropConnection));
            assert_eq!(plan.visit(rule.point), None);
        }
    }
}
