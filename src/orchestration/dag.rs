//! 基于 petgraph 的 DAG 校验与分层（拓扑排序）。
use crate::config::PipelineDef;
use anyhow::Context;
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use std::collections::HashMap;

/// 校验 pipeline：step id 唯一、依赖存在、无环。
pub fn validate_pipeline(name: &str, def: &PipelineDef) -> anyhow::Result<()> {
    let layers = build_layers(def).with_context(|| format!("pipeline [{}] 非法", name))?;
    anyhow::ensure!(!layers.is_empty(), "pipeline [{}] 没有任何 step", name);
    Ok(())
}

/// 返回按层分组的 step 索引：同层无依赖关系，可并行执行；层间严格先后。
pub fn build_layers(def: &PipelineDef) -> anyhow::Result<Vec<Vec<usize>>> {
    let n = def.steps.len();
    let mut index_of: HashMap<&str, usize> = HashMap::new();
    for (i, s) in def.steps.iter().enumerate() {
        anyhow::ensure!(!s.id.is_empty(), "step id 不能为空");
        anyhow::ensure!(
            index_of.insert(s.id.as_str(), i).is_none(),
            "step id 重复: {}",
            s.id
        );
    }

    let mut graph = DiGraph::<usize, ()>::new();
    let nodes: Vec<NodeIndex> = (0..n).map(|i| graph.add_node(i)).collect();
    let mut indegree = vec![0usize; n];
    for (i, s) in def.steps.iter().enumerate() {
        for dep in &s.depends_on {
            let &d = index_of
                .get(dep.as_str())
                .with_context(|| format!("step [{}] 依赖了不存在的 step [{}]", s.id, dep))?;
            graph.add_edge(nodes[d], nodes[i], ());
            indegree[i] += 1;
        }
    }
    // 无环校验
    toposort(&graph, None).map_err(|_| anyhow::anyhow!("pipeline 存在循环依赖"))?;

    // 按最长路径分层
    let mut level = vec![0usize; n];
    let order = toposort(&graph, None).unwrap();
    for idx in &order {
        let i = graph[*idx];
        for dep in &def.steps[i].depends_on {
            let d = index_of[dep.as_str()];
            level[i] = level[i].max(level[d] + 1);
        }
    }
    let max_level = *level.iter().max().unwrap_or(&0);
    let mut layers = vec![Vec::new(); max_level + 1];
    for (i, &lv) in level.iter().enumerate() {
        layers[lv].push(i);
    }
    Ok(layers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{StepDef, StepType};

    fn step(id: &str, deps: &[&str]) -> StepDef {
        StepDef {
            id: id.into(),
            step_type: StepType::Script,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            config: Default::default(),
        }
    }

    #[test]
    fn layers_parallel_roots() {
        let def = PipelineDef {
            strategy: Default::default(),
            steps: vec![step("a", &[]), step("b", &[]), step("c", &["a", "b"])],
        };
        let layers = build_layers(&def).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].len(), 2);
        assert_eq!(layers[1], vec![2]);
    }

    #[test]
    fn rejects_cycle() {
        let def = PipelineDef {
            strategy: Default::default(),
            steps: vec![step("a", &["b"]), step("b", &["a"])],
        };
        assert!(build_layers(&def).is_err());
    }

    #[test]
    fn rejects_missing_dep() {
        let def = PipelineDef {
            strategy: Default::default(),
            steps: vec![step("a", &["nope"])],
        };
        assert!(build_layers(&def).is_err());
    }
}
