use crate::capabilities::CapabilitySet;
use thiserror::Error;

pub const MAX_SPAWN_DEPTH: u32 = 4;

#[derive(Debug, Error)]
pub enum SpawnError {
    #[error("capability escalation: child requested {requested:?}, parent has {allowed:?}")]
    CapabilityEscalation {
        requested: CapabilitySet,
        allowed: CapabilitySet,
    },

    #[error("insufficient fuel: requested {requested}, remaining {remaining}")]
    InsufficientFuel {
        requested: u64,
        remaining: u64,
    },

    #[error("spawn depth exceeded: parent depth {current} + 1 exceeds max {max}")]
    DepthExceeded {
        current: u32,
        max: u32,
    },
}

#[derive(Debug, Clone)]
pub struct SpawnContext {
    pub capability_mask: CapabilitySet,
    pub remaining_fuel: u64,
    pub spawn_depth: u32,
}

impl SpawnContext {
    pub fn new(capability_mask: CapabilitySet, remaining_fuel: u64, spawn_depth: u32) -> Self {
        Self {
            capability_mask,
            remaining_fuel,
            spawn_depth,
        }
    }

    pub fn deduct_fuel(&mut self, amount: u64) {
        self.remaining_fuel = self.remaining_fuel.saturating_sub(amount);
    }

    pub fn child_context(&self, requested_fuel: u64) -> Result<SpawnContext, SpawnError> {
        if self.spawn_depth >= MAX_SPAWN_DEPTH {
            return Err(SpawnError::DepthExceeded {
                current: self.spawn_depth,
                max: MAX_SPAWN_DEPTH,
            });
        }
        if requested_fuel > self.remaining_fuel {
            return Err(SpawnError::InsufficientFuel {
                requested: requested_fuel,
                remaining: self.remaining_fuel,
            });
        }
        Ok(SpawnContext {
            capability_mask: self.capability_mask.clone(),
            remaining_fuel: requested_fuel,
            spawn_depth: self.spawn_depth + 1,
        })
    }
}

pub struct AgentPool {}

impl AgentPool {
    pub fn new() -> Self {
        Self {}
    }

    pub fn spawn_agent(
        &self,
        parent_ctx: &mut SpawnContext,
        requested_caps: CapabilitySet,
        requested_fuel: u64,
    ) -> Result<SpawnContext, SpawnError> {
        // 1. Capability subset — child cannot exceed parent
        if !parent_ctx.capability_mask.contains_all(&requested_caps) {
            return Err(SpawnError::CapabilityEscalation {
                requested: requested_caps,
                allowed: parent_ctx.capability_mask.clone(),
            });
        }
        // 2. Fuel carving — atomic deduction from parent
        if requested_fuel > parent_ctx.remaining_fuel {
            return Err(SpawnError::InsufficientFuel {
                requested: requested_fuel,
                remaining: parent_ctx.remaining_fuel,
            });
        }
        // 3. Spawn depth cap
        if parent_ctx.spawn_depth >= MAX_SPAWN_DEPTH {
            return Err(SpawnError::DepthExceeded {
                current: parent_ctx.spawn_depth,
                max: MAX_SPAWN_DEPTH,
            });
        }
        parent_ctx.deduct_fuel(requested_fuel);
        Ok(SpawnContext {
            capability_mask: requested_caps,
            remaining_fuel: requested_fuel,
            spawn_depth: parent_ctx.spawn_depth + 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::Capability;

    #[test]
    fn test_agent_spawn_with_valid_caps_succeeds() {
        let mut parent = SpawnContext::new(
            CapabilitySet::agent2_default(),
            1000,
            0,
        );
        let pool = AgentPool::new();
        let child_caps = CapabilitySet::new(vec![Capability::FileRead, Capability::FileWrite]);
        let result = pool.spawn_agent(&mut parent, child_caps, 200);
        assert!(result.is_ok(), "Valid spawn should succeed: {:?}", result.err());
        let child = result.unwrap();
        assert_eq!(child.spawn_depth, 1);
        assert_eq!(child.remaining_fuel, 200);
        assert_eq!(parent.remaining_fuel, 800);
    }

    #[test]
    fn test_capability_escalation_is_denied() {
        let mut parent = SpawnContext::new(
            CapabilitySet::agent1_default(),
            1000,
            0,
        );
        let pool = AgentPool::new();
        // Agent1 has only read — requesting write is escalation
        let child_caps = CapabilitySet::new(vec![Capability::FileWrite]);
        let result = pool.spawn_agent(&mut parent, child_caps, 100);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), SpawnError::CapabilityEscalation { .. }));
    }

    #[test]
    fn test_insufficient_fuel_is_denied() {
        let mut parent = SpawnContext::new(
            CapabilitySet::agent2_default(),
            500,
            0,
        );
        let pool = AgentPool::new();
        let child_caps = CapabilitySet::new(vec![Capability::FileRead]);
        let result = pool.spawn_agent(&mut parent, child_caps, 600);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), SpawnError::InsufficientFuel { .. }));
    }

    #[test]
    fn test_depth_limit_is_enforced() {
        let mut parent = SpawnContext::new(
            CapabilitySet::agent2_default(),
            1000,
            MAX_SPAWN_DEPTH, // already at max
        );
        let pool = AgentPool::new();
        let child_caps = CapabilitySet::new(vec![Capability::FileRead]);
        let result = pool.spawn_agent(&mut parent, child_caps, 100);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), SpawnError::DepthExceeded { .. }));
    }
}
