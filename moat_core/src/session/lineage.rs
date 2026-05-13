use super::store::SessionStore;
use anyhow::Result;

pub struct SessionLineage;

impl SessionLineage {
    pub fn get_lineage(store: &SessionStore, session_id: &str) -> Result<Vec<String>> {
        let mut chain = vec![session_id.to_string()];
        let mut current = session_id.to_string();

        for _ in 0..100 {
            let sessions = store.list_recent_sessions(1000)?;
            let current_session = sessions.iter().find(|s| s.id == current);

            match current_session.and_then(|s| s.parent_id.clone()) {
                Some(parent_id) => {
                    chain.push(parent_id.clone());
                    current = parent_id;
                }
                None => break,
            }
        }

        chain.reverse();
        Ok(chain)
    }
}