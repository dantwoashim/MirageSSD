use mirage_types::MirageError;

use crate::GhostKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Adaptation {
    pub window_target: usize,
    pub protected_target: usize,
    min: usize,
    max: usize,
    max_step: usize,
}
impl Adaptation {
    pub fn new(
        window_target: usize,
        protected_target: usize,
        min: usize,
        max: usize,
        max_step: usize,
    ) -> Result<Self, MirageError> {
        if min > max
            || max_step == 0
            || !(min..=max).contains(&window_target)
            || !(min..=max).contains(&protected_target)
        {
            return Err(MirageError::invalid_argument(
                "adaptation bounds are invalid",
            ));
        }
        Ok(Self {
            window_target,
            protected_target,
            min,
            max,
            max_step,
        })
    }
    pub fn ghost_hit(&mut self, kind: GhostKind) {
        match kind {
            GhostKind::Window => {
                self.window_target = self
                    .window_target
                    .saturating_add(self.max_step)
                    .min(self.max)
            }
            GhostKind::Protected => {
                self.protected_target = self
                    .protected_target
                    .saturating_add(self.max_step)
                    .min(self.max)
            }
            GhostKind::Probationary => {
                self.window_target = self
                    .window_target
                    .saturating_sub(self.max_step)
                    .max(self.min);
                self.protected_target = self
                    .protected_target
                    .saturating_sub(self.max_step)
                    .max(self.min);
            }
        }
    }
}
