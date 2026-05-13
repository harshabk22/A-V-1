#[derive(Debug, Clone, PartialEq)]
pub enum RouteTarget {
    Agent1,
    Agent2,
}

#[derive(Debug, Clone)]
pub struct RouteDecision {
    pub target: RouteTarget,
    pub confidence: f32,
    pub method: RouteMethod,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub enum RouteMethod {
    RegexExact,
    RegexWeak,
    LlmFallback,
    Delegation,
}

impl RouteDecision {
    pub fn agent1_strong(reason: &str) -> Self {
        Self { target: RouteTarget::Agent1, confidence: 0.95, method: RouteMethod::RegexExact, reason: reason.into() }
    }

    pub fn agent2_strong(reason: &str) -> Self {
        Self { target: RouteTarget::Agent2, confidence: 0.95, method: RouteMethod::RegexExact, reason: reason.into() }
    }

    pub fn agent1_weak(confidence: f32, reason: &str) -> Self {
        Self { target: RouteTarget::Agent1, confidence, method: RouteMethod::RegexWeak, reason: reason.into() }
    }

    pub fn agent2_weak(confidence: f32, reason: &str) -> Self {
        Self { target: RouteTarget::Agent2, confidence, method: RouteMethod::RegexWeak, reason: reason.into() }
    }

    pub fn needs_llm_fallback(&self) -> bool {
        self.confidence < 0.65
    }
}