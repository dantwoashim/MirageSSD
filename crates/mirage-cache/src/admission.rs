use mirage_types::{MirageError, PageHash};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdmissionContext {
    pub incoming_freq: u32,
    pub victim_freq: u32,
    pub incoming_miss_cost: u32,
    pub victim_miss_cost: u32,
    pub incoming_capsule_probability: f32,
    pub prefetched: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionWeights {
    pub frequency: u32,
    pub miss_cost: u32,
    pub capsule: u32,
    pub prefetch_penalty: u32,
}
impl Default for AdmissionWeights {
    fn default() -> Self {
        Self {
            frequency: 100,
            miss_cost: 1,
            capsule: 100,
            prefetch_penalty: 50,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionDecision {
    Admit,
    RejectSpeculative,
}

impl AdmissionWeights {
    pub fn decide(
        self,
        incoming: PageHash,
        victim: PageHash,
        context: AdmissionContext,
        blocking: bool,
    ) -> Result<AdmissionDecision, MirageError> {
        if !context.incoming_capsule_probability.is_finite()
            || !(0.0..=1.0).contains(&context.incoming_capsule_probability)
        {
            return Err(MirageError::invalid_argument(
                "capsule probability must be finite and normalized",
            ));
        }
        if blocking {
            return Ok(AdmissionDecision::Admit);
        }
        let probability = (context.incoming_capsule_probability * 10_000.0).round() as u64;
        let incoming_score = u64::from(context.incoming_freq)
            .saturating_mul(u64::from(self.frequency))
            .saturating_add(
                u64::from(context.incoming_miss_cost).saturating_mul(u64::from(self.miss_cost)),
            )
            .saturating_add(probability.saturating_mul(u64::from(self.capsule)) / 10_000)
            .saturating_sub(if context.prefetched {
                u64::from(self.prefetch_penalty)
            } else {
                0
            });
        let victim_score = u64::from(context.victim_freq)
            .saturating_mul(u64::from(self.frequency))
            .saturating_add(
                u64::from(context.victim_miss_cost).saturating_mul(u64::from(self.miss_cost)),
            );
        Ok(
            if incoming_score > victim_score
                || (incoming_score == victim_score && incoming < victim)
            {
                AdmissionDecision::Admit
            } else {
                AdmissionDecision::RejectSpeculative
            },
        )
    }
}
