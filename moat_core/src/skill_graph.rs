use std::collections::{HashMap, VecDeque};
use thiserror::Error;

pub const MAX_DEPENDENCY_DEPTH: u32 = 5;

#[derive(Debug, Clone)]
pub struct SkillNode {
    pub name: String,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("circular dependency detected: cycle through {0:?}")]
    CircularDependency(Vec<String>),

    #[error("dependency depth {depth} exceeds maximum {MAX_DEPENDENCY_DEPTH} for skill '{skill}'")]
    DepthExceeded { skill: String, depth: u32 },

    #[error("unknown dependency '{dependency}' referenced by skill '{skill}'")]
    UnknownDependency { skill: String, dependency: String },
}

/// Kahn's topological sort for skill dependency resolution.
/// Returns the skills in installation order (dependencies first).
///
/// Circular dependency detection runs at install time (NOT at execute time).
/// A skill with a circular dependency must never reach the executable state.
pub fn resolve_install_order(
    skills: &[SkillNode],
) -> Result<Vec<SkillNode>, InstallError> {
    // Build adjacency and in-degree maps
    let name_to_idx: HashMap<&str, usize> = skills
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.as_str(), i))
        .collect();

    let mut in_degree = vec![0usize; skills.len()];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); skills.len()];

    for (i, skill) in skills.iter().enumerate() {
        // Check max depth via DFS
        let depth = dfs_max_depth(&skills, &name_to_idx, i, &mut HashMap::new())?;
        if depth > MAX_DEPENDENCY_DEPTH {
            return Err(InstallError::DepthExceeded {
                skill: skill.name.clone(),
                depth,
            });
        }

        for dep_name in &skill.dependencies {
            let dep_idx = name_to_idx.get(dep_name.as_str()).ok_or_else(|| {
                InstallError::UnknownDependency {
                    skill: skill.name.clone(),
                    dependency: dep_name.clone(),
                }
            })?;
            adj[*dep_idx].push(i); // dep must come before skill
            in_degree[i] += 1;
        }
    }

    // Kahn's algorithm: queue nodes with in-degree 0
    let mut queue: VecDeque<usize> = in_degree
        .iter()
        .enumerate()
        .filter(|(_, &d)| d == 0)
        .map(|(i, _)| i)
        .collect();

    let mut order = Vec::with_capacity(skills.len());

    while let Some(node) = queue.pop_front() {
        order.push(skills[node].clone());

        for &next in &adj[node] {
            in_degree[next] -= 1;
            if in_degree[next] == 0 {
                queue.push_back(next);
            }
        }
    }

    // If order.len() != skills.len(), there's a cycle
    if order.len() < skills.len() {
        let unresolved: Vec<String> = skills
            .iter()
            .enumerate()
            .filter(|(i, _)| in_degree[*i] > 0)
            .map(|(_, s)| s.name.clone())
            .collect();
        return Err(InstallError::CircularDependency(unresolved));
    }

    Ok(order)
}

/// DFS to find the maximum dependency chain depth for a skill.
fn dfs_max_depth(
    skills: &[SkillNode],
    name_to_idx: &HashMap<&str, usize>,
    current: usize,
    visited: &mut HashMap<usize, u32>,
) -> Result<u32, InstallError> {
    if let Some(&cached) = visited.get(&current) {
        return Ok(cached);
    }

    // Mark as 0 to detect cycles during DFS (temporary sentinel)
    visited.insert(current, 0);
    let mut max_child = 0u32;
    for dep in &skills[current].dependencies {
        if let Some(&dep_idx) = name_to_idx.get(dep.as_str()) {
            if visited.get(&dep_idx) == Some(&0) {
                // Cycle detected — this will be caught by Kahn's sort,
                // but we surface it here too for early detection
                return Err(InstallError::CircularDependency(vec![
                    skills[current].name.clone(),
                    dep.clone(),
                ]));
            }
            let child_depth = dfs_max_depth(skills, name_to_idx, dep_idx, visited)?;
            max_child = max_child.max(child_depth);
        }
    }
    let depth = 1 + max_child;
    visited.insert(current, depth);
    Ok(depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kahn_linear_dependencies() {
        let skills = vec![
            SkillNode { name: "base".into(), dependencies: vec![] },
            SkillNode { name: "mid".into(), dependencies: vec!["base".into()] },
            SkillNode { name: "top".into(), dependencies: vec!["mid".into()] },
        ];
        let order = resolve_install_order(&skills).expect("linear should work");
        let names: Vec<&str> = order.iter().map(|s| s.name.as_str()).collect();
        let base_pos = names.iter().position(|&n| n == "base").unwrap();
        let mid_pos = names.iter().position(|&n| n == "mid").unwrap();
        let top_pos = names.iter().position(|&n| n == "top").unwrap();
        assert!(base_pos < mid_pos, "base must install before mid");
        assert!(mid_pos < top_pos, "mid must install before top");
    }

    #[test]
    fn test_circular_dependency_is_rejected() {
        let skills = vec![
            SkillNode { name: "a".into(), dependencies: vec!["b".into()] },
            SkillNode { name: "b".into(), dependencies: vec!["a".into()] },
        ];
        let result = resolve_install_order(&skills);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InstallError::CircularDependency(_)));
    }

    #[test]
    fn test_unknown_dependency_is_rejected() {
        let skills = vec![
            SkillNode { name: "a".into(), dependencies: vec!["nonexistent".into()] },
        ];
        let result = resolve_install_order(&skills);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InstallError::UnknownDependency { .. }));
    }

    #[test]
    fn test_depth_limit_is_enforced() {
        // Create a chain longer than MAX_DEPENDENCY_DEPTH
        let mut skills = Vec::new();
        skills.push(SkillNode { name: "s0".into(), dependencies: vec![] });
        for i in 1..=MAX_DEPENDENCY_DEPTH + 1 {
            let prev = format!("s{}", i.saturating_sub(1));
            skills.push(SkillNode {
                name: format!("s{}", i),
                dependencies: vec![prev],
            });
        }
        let result = resolve_install_order(&skills);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), InstallError::DepthExceeded { .. }));
    }

    #[test]
    fn test_diamond_dependencies_resolve_correctly() {
        // A depends on B and C; B and C depend on D
        let skills = vec![
            SkillNode { name: "d".into(), dependencies: vec![] },
            SkillNode { name: "b".into(), dependencies: vec!["d".into()] },
            SkillNode { name: "c".into(), dependencies: vec!["d".into()] },
            SkillNode { name: "a".into(), dependencies: vec!["b".into(), "c".into()] },
        ];
        let order = resolve_install_order(&skills).expect("diamond should work");
        let names: Vec<&str> = order.iter().map(|s| s.name.as_str()).collect();
        let d_pos = names.iter().position(|&n| n == "d").unwrap();
        let b_pos = names.iter().position(|&n| n == "b").unwrap();
        let c_pos = names.iter().position(|&n| n == "c").unwrap();
        let a_pos = names.iter().position(|&n| n == "a").unwrap();
        assert!(d_pos < b_pos && d_pos < c_pos, "d must install before b and c");
        assert!(b_pos < a_pos && c_pos < a_pos, "b and c must install before a");
    }
}
