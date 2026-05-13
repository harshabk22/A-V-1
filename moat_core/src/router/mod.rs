pub mod classifier;
pub mod decision;
pub mod llm_fallback;

pub use decision::{RouteDecision, RouteMethod, RouteTarget};

use classifier::RegexClassifier;
use llm_fallback::LlmRouter;
use crate::llm::LlmClient;
use anyhow::Result;

pub struct Router {
    classifier: RegexClassifier,
    llm_router: LlmRouter,
}

impl Router {
    pub fn new(llm: LlmClient) -> Result<Self> {
        Ok(Self {
            classifier: RegexClassifier::default(),
            llm_router: LlmRouter::new(llm),
        })
    }

    pub async fn route(&self, query: &str, context_hint: Option<&str>) -> RouteDecision {
        let decision = self.classifier.classify(query);

        if decision.needs_llm_fallback() {
            println!("[Router] Low confidence ({:.2}) — using LLM fallback", decision.confidence);
            match self.llm_router.classify(query, context_hint).await {
                Ok(llm_decision) => llm_decision,
                Err(e) => {
                    eprintln!("[Router] LLM fallback failed ({}), defaulting to Agent1", e);
                    RouteDecision::agent1_weak(0.50, "LLM fallback unavailable")
                }
            }
        } else {
            println!("[Router] Classified → {:?} (conf: {:.2}, method: {:?})",
                decision.target, decision.confidence, decision.method);
            decision
        }
    }

    pub fn make_delegation_decision(reason: &str) -> RouteDecision {
        RouteDecision {
            target: RouteTarget::Agent2,
            confidence: 1.0,
            method: RouteMethod::Delegation,
            reason: format!("Agent1 delegated: {}", reason),
        }
    }
}