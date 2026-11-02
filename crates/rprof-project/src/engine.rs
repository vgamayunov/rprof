use crate::*;
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap};

const MAX_PHASES: usize = 4096;
pub fn digest(value: &impl Serialize) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}
fn integer(s: &str) -> Result<u64> {
    ensure!(
        !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit()),
        "expected a decimal u64 string (1..=20 digits)"
    );
    Ok(s.parse()?)
}
fn label(s: &str, limit: usize) -> Result<()> {
    ensure!(
        !s.trim().is_empty() && s.len() <= limit,
        "text must contain 1..={limit} bytes"
    );
    Ok(())
}
fn identifier(s: &str) -> Result<()> {
    label(s, 128)?;
    ensure!(
        s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)),
        "invalid identifier {s:?}"
    );
    Ok(())
}
fn hash(s: &str) -> Result<()> {
    ensure!(
        s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()),
        "expected SHA-256 hex digest"
    );
    Ok(())
}
fn provenance(p: &Provenance) -> Result<()> {
    label(&p.source, 2048)?;
    label(&p.date, 64)?;
    label(&p.conditions, 4096)
}
fn profile(p: &HardwareProfile) -> Result<()> {
    ensure!(
        p.version == 1,
        "unsupported hardware profile version {}",
        p.version
    );
    identifier(&p.id)?;
    label(&p.name, 512)?;
    provenance(&p.provenance)?;
    ensure!(
        (1..=1_000_000).contains(&p.nodes),
        "hardware node count must be 1..=1000000"
    );
    ensure!(
        (1..=65536).contains(&p.placement.ranks_per_node)
            && (1..=65536).contains(&p.placement.threads_per_rank)
            && p.placement.gpus_per_rank <= 1024,
        "invalid placement"
    );
    ensure!(p.resources.len() <= 64, "at most 64 resources per profile");
    for (name, r) in &p.resources {
        identifier(name)?;
        label(&r.description, 2048)?;
        ensure!(
            (1..=1024).contains(&r.slots_per_node),
            "resource slots must be 1..=1024"
        );
        if let Some(capacity) = &r.capacity_bytes_per_node {
            integer(capacity)?;
        }
        ensure!(
            r.metrics.len() <= 64 && r.features.len() <= 128,
            "resource metadata limit exceeded"
        );
        for feature in &r.features {
            label(feature, 128)?;
        }
        for (name, m) in &r.metrics {
            identifier(name)?;
            provenance(&m.provenance)?;
            ensure!(
                [m.low, m.central, m.high]
                    .iter()
                    .all(|n| n.is_finite() && *n >= 0.0 && *n <= 1e24)
                    && m.low <= m.central
                    && m.central <= m.high,
                "invalid metric bounds for {name}"
            );
            ensure!(
                m.unit == Unit::Nanoseconds || m.low >= 1e-12,
                "throughput must be positive for {name}"
            );
        }
    }
    Ok(())
}

struct Plan {
    spans: Vec<usize>,
    starts: Vec<u64>,
    durations: Vec<u64>,
    after: Vec<Vec<usize>>,
    order: Vec<usize>,
    lag: Vec<u64>,
    tail: u64,
    covered: u64,
    window: u64,
}
type Slots = BinaryHeap<Reverse<(u64, u32, Option<usize>)>>;
fn slots(profile: &HardwareProfile) -> BTreeMap<String, Slots> {
    profile
        .resources
        .iter()
        .map(|(key, r)| {
            (
                key.clone(),
                (0..r.slots_per_node)
                    .map(|i| Reverse((0, i, None)))
                    .collect(),
            )
        })
        .collect()
}
fn plan(request: &Request) -> Result<Plan> {
    let b = &request.baseline;
    let w = &request.workload;
    ensure!(w.version == 1, "unsupported workload model version");
    ensure!(
        w.baseline_sha256 == b.capture_sha256 && w.domain == b.domain,
        "workload model does not match the capture hash and clock domain"
    );
    label(&w.name, 512)?;
    label(&w.assumptions, 4096)?;
    ensure!(
        !w.phases.is_empty() && w.phases.len() <= MAX_PHASES,
        "workload requires 1..={MAX_PHASES} phases"
    );
    ensure!(
        w.phases.iter().map(|p| p.after.len()).sum::<usize>() <= 65536,
        "at most 65536 explicit dependency edges"
    );
    ensure!(
        w.memory.len() <= 64 && w.required_features.len() <= 64,
        "requirement limit exceeded"
    );
    for (name, m) in &w.memory {
        identifier(name)?;
        integer(&m.bytes_per_source_node)?;
    }
    for (name, features) in &w.required_features {
        identifier(name)?;
        ensure!(features.len() <= 128, "feature limit exceeded");
        for f in features {
            label(f, 128)?;
        }
    }
    for s in [
        &w.unmodeled_assumption,
        &w.scaling_assumption,
        &w.placement_assumption,
    ] {
        ensure!(s.len() <= 4096, "assumption text too large");
    }
    if w.unmodeled == Unmodeled::Unchanged {
        label(&w.unmodeled_assumption, 4096)?;
    }
    let offset = integer(&w.window_start_ns)?;
    let window = integer(&w.window_duration_ns)?;
    ensure!(
        window > 0
            && offset
                .checked_add(window)
                .is_some_and(|end| end <= integer(&b.duration_ns).unwrap_or(0)),
        "model window exceeds the captured domain"
    );
    let evidence: BTreeMap<_, _> = b
        .spans
        .iter()
        .enumerate()
        .map(|(i, s)| (s.id.as_str(), i))
        .collect();
    ensure!(
        evidence.len() == b.spans.len(),
        "duplicate evidence span IDs"
    );
    let mut ids = BTreeMap::new();
    let mut used = BTreeSet::new();
    let mut spans = Vec::new();
    let mut starts = Vec::new();
    let mut durations = Vec::new();
    let mut lanes: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, p) in w.phases.iter().enumerate() {
        identifier(&p.id)?;
        label(&p.span_id, 128)?;
        label(&p.name, 512)?;
        label(&p.assumption, 4096)?;
        match &p.model {
            PhaseModel::Unchanged => (),
            PhaseModel::Throughput { metric } => identifier(metric)?,
            PhaseModel::Transfer {
                bandwidth_metric,
                latency_metric,
                bytes,
                operations,
            } => {
                identifier(bandwidth_metric)?;
                if let Some(metric) = latency_metric {
                    identifier(metric)?;
                }
                integer(bytes)?;
                integer(operations)?;
            }
            PhaseModel::Roofline {
                compute_metric,
                bandwidth_metric,
                work,
                bytes,
            } => {
                identifier(compute_metric)?;
                identifier(bandwidth_metric)?;
                integer(work)?;
                integer(bytes)?;
            }
            PhaseModel::Unsupported { reason } => label(reason, 4096)?,
        }
        ensure!(
            ids.insert(p.id.as_str(), index).is_none(),
            "duplicate phase {}",
            p.id
        );
        ensure!(
            used.insert(&p.span_id),
            "an observed span cannot be counted twice"
        );
        let s = *evidence
            .get(p.span_id.as_str())
            .with_context(|| format!("span {} is absent from retained evidence", p.span_id))?;
        let span = &b.spans[s];
        let start = integer(&span.start_ns)?
            .checked_sub(offset)
            .context("phase begins before model window")?;
        let duration = integer(&span.duration_ns)?;
        ensure!(
            start.checked_add(duration).is_some_and(|end| end <= window),
            "phase {} exceeds model window",
            p.id
        );
        ensure!(
            !span.enclosing || w.phases.len() == 1,
            "enclosing workload spans cannot be combined with their contents"
        );
        if let Some(resource) = &p.resource {
            identifier(resource)?;
            ensure!(
                request.source.resources.contains_key(resource),
                "source profile lacks resource {resource}"
            );
        }
        ensure!(p.after.len() <= 256, "at most 256 predecessors per phase");
        if let NodeScaling::Power { exponent } = p.node_scaling {
            ensure!((-2..=2).contains(&exponent), "node exponent must be -2..=2");
        }
        spans.push(s);
        starts.push(start);
        durations.push(duration);
        lanes.entry(&span.lane).or_default().push(index);
    }
    let mut after = vec![Vec::new(); w.phases.len()];
    for (index, p) in w.phases.iter().enumerate() {
        for id in &p.after {
            identifier(id)?;
            let &dep = ids
                .get(id.as_str())
                .with_context(|| format!("unknown predecessor {id}"))?;
            ensure!(dep != index, "self dependency");
            after[index].push(dep);
        }
    }
    // Recorded execution-lane order is preserved. Nested inclusive scopes are
    // rejected rather than being scheduled as independent work.
    for phases in lanes.values_mut() {
        phases.sort_by_key(|i| (starts[*i], *i));
        for pair in phases.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            ensure!(
                starts[a] + durations[a] <= starts[b],
                "selected phases overlap on execution lane; select non-overlapping leaf spans"
            );
            after[b].push(a);
        }
    }
    let mut children = vec![Vec::new(); after.len()];
    let mut indegree = Vec::new();
    let mut ready = BTreeSet::new();
    for (i, deps) in after.iter_mut().enumerate() {
        deps.sort();
        deps.dedup();
        indegree.push(deps.len());
        if deps.is_empty() {
            ready.insert((starts[i], i));
        }
        for &dep in deps.iter() {
            ensure!(
                starts[dep] + durations[dep] <= starts[i],
                "finish-to-start dependency contradicts source timing for {}",
                w.phases[i].id
            );
            children[dep].push(i);
        }
    }
    let mut order = Vec::new();
    while let Some((_, i)) = ready.pop_first() {
        order.push(i);
        for &next in &children[i] {
            indegree[next] -= 1;
            if indegree[next] == 0 {
                ready.insert((starts[next], next));
            }
        }
    }
    ensure!(order.len() == w.phases.len(), "phase graph has a cycle");
    let mut source_slots = slots(&request.source);
    let mut lag = vec![0; after.len()];
    for &i in &order {
        let mut available = after[i]
            .iter()
            .map(|d| starts[*d] + durations[*d])
            .max()
            .unwrap_or(0);
        let slot = w.phases[i]
            .resource
            .as_ref()
            .map(|r| source_slots.get_mut(r).unwrap().pop().unwrap().0);
        if let Some((at, _, _)) = slot {
            available = available.max(at);
        }
        ensure!(
            available <= starts[i],
            "source resource capacity cannot reproduce phase {}; review concurrency and scope selection",
            w.phases[i].id
        );
        lag[i] = starts[i] - available;
        if let Some((_, slot, _)) = slot {
            source_slots
                .get_mut(w.phases[i].resource.as_ref().unwrap())
                .unwrap()
                .push(Reverse((starts[i] + durations[i], slot, Some(i))));
        }
    }
    let mut intervals: Vec<_> = starts
        .iter()
        .zip(&durations)
        .map(|(s, d)| (*s, s + d))
        .collect();
    intervals.sort();
    let mut covered = 0;
    let mut end = 0;
    for (s, e) in intervals {
        if e > end {
            covered += e - s.max(end);
            end = e;
        }
    }
    Ok(Plan {
        spans,
        starts,
        durations,
        after,
        order,
        lag,
        tail: window - end,
        covered,
        window,
    })
}

#[derive(Clone, Copy)]
enum Bound {
    Fast,
    Central,
    Slow,
}
fn metric<'a>(resource: &'a Resource, key: &str, unit: Option<Unit>) -> Result<&'a Metric> {
    let m = resource
        .metrics
        .get(key)
        .with_context(|| format!("missing metric {key}"))?;
    if let Some(unit) = unit {
        ensure!(m.unit == unit, "wrong unit for {key}");
    } else {
        ensure!(
            m.unit != Unit::Nanoseconds,
            "throughput model requires a throughput metric"
        );
    }
    Ok(m)
}
fn value(m: &Metric, bound: Bound) -> f64 {
    match (bound, m.unit) {
        (Bound::Central, _) => m.central,
        (Bound::Fast, Unit::Nanoseconds)
        | (Bound::Slow, Unit::WorkPerSecond | Unit::BytesPerSecond) => m.low,
        _ => m.high,
    }
}
fn cost(model: &PhaseModel, r: &Resource, bound: Bound) -> Result<(f64, String)> {
    match model {
        PhaseModel::Throughput { metric: key } => Ok((
            1.0 / value(metric(r, key, None)?, bound),
            format!("throughput: {key}"),
        )),
        PhaseModel::Transfer {
            bandwidth_metric,
            latency_metric,
            bytes,
            operations,
        } => {
            let operations = integer(operations)?;
            let transfer = integer(bytes)? as f64
                / value(
                    metric(r, bandwidth_metric, Some(Unit::BytesPerSecond))?,
                    bound,
                );
            let latency = if let Some(key) = latency_metric {
                operations as f64 * value(metric(r, key, Some(Unit::Nanoseconds))?, bound) / 1e9
            } else {
                0.0
            };
            Ok((
                transfer + latency,
                if latency >= transfer {
                    "latency"
                } else {
                    "bandwidth"
                }
                .into(),
            ))
        }
        PhaseModel::Roofline {
            compute_metric,
            bandwidth_metric,
            work,
            bytes,
        } => {
            ensure!(
                matches!(r.kind, ResourceKind::Cpu | ResourceKind::Gpu),
                "roofline model requires CPU or GPU resource"
            );
            let compute = integer(work)? as f64
                / value(metric(r, compute_metric, Some(Unit::WorkPerSecond))?, bound);
            let memory = integer(bytes)? as f64
                / value(
                    metric(r, bandwidth_metric, Some(Unit::BytesPerSecond))?,
                    bound,
                );
            Ok((
                compute.max(memory),
                if compute >= memory {
                    "compute"
                } else {
                    "memory bandwidth"
                }
                .into(),
            ))
        }
        PhaseModel::Unsupported { reason } => bail!("unsupported phase: {reason}"),
        PhaseModel::Unchanged => Ok((1.0, "unchanged hardware sensitivity".into())),
    }
}
fn node_factor(scaling: Scaling, model: NodeScaling, source: u32, target: u32) -> f64 {
    let ratio = target as f64 / source as f64;
    match model {
        NodeScaling::Fixed => 1.0,
        NodeScaling::Distributed => {
            if scaling == Scaling::Strong {
                1.0 / ratio
            } else {
                1.0
            }
        }
        NodeScaling::Power { exponent } => match exponent {
            -2 => 1.0 / (ratio * ratio),
            -1 => 1.0 / ratio,
            0 => 1.0,
            1 => ratio,
            _ => ratio * ratio,
        },
    }
}
fn scale(duration: u64, factor: f64) -> Result<u64> {
    ensure!(
        factor.is_finite() && factor > 0.0,
        "invalid modeled cost ratio"
    );
    if factor == 1.0 || duration == 0 {
        return Ok(duration);
    }
    let result = duration as f64 * factor;
    ensure!(
        result.is_finite() && result < (u64::MAX as f64),
        "projected duration exceeds u64 range"
    );
    Ok(result.round().max(1.0) as u64)
}
fn durations(
    request: &Request,
    target: &HardwareProfile,
    nodes: u32,
    plan: &Plan,
    bound: Bound,
) -> Result<(Vec<u64>, Vec<String>)> {
    let mut durations = Vec::new();
    let mut bottlenecks = Vec::new();
    let same = request.source.id == target.id && request.source.resources == target.resources;
    for (i, phase) in request.workload.phases.iter().enumerate() {
        if let Some(resource) = &phase.resource {
            let target_resource = target
                .resources
                .get(resource)
                .with_context(|| format!("target lacks resource {resource}"))?;
            ensure!(
                request.source.resources[resource].kind == target_resource.kind,
                "resource kind changed for {resource}"
            );
        }
        let (factor, bottleneck) = match &phase.model {
            PhaseModel::Unchanged => (1.0, "unchanged hardware sensitivity".into()),
            PhaseModel::Unsupported { reason } => bail!("{}: {reason}", phase.id),
            _ => {
                let resource = phase
                    .resource
                    .as_ref()
                    .context("modeled phase requires a resource")?;
                let s = &request.source.resources[resource];
                let t = target
                    .resources
                    .get(resource)
                    .with_context(|| format!("target lacks resource {resource}"))?;
                if let PhaseModel::Throughput { metric: key } = &phase.model {
                    ensure!(
                        metric(s, key, None)?.unit == metric(t, key, None)?.unit,
                        "source and target throughput units differ for {key}"
                    );
                }
                ensure!(s.kind == t.kind, "resource kind changed for {resource}");
                let opposite = match bound {
                    Bound::Fast => Bound::Slow,
                    Bound::Central => Bound::Central,
                    Bound::Slow => Bound::Fast,
                };
                let (sc, _) = cost(&phase.model, s, opposite)
                    .with_context(|| format!("source phase {}", phase.id))?;
                let (tc, label) = cost(&phase.model, t, bound)
                    .with_context(|| format!("target phase {}", phase.id))?;
                ensure!(
                    sc > 0.0 && tc > 0.0 && sc.is_finite() && tc.is_finite(),
                    "phase {} has no positive finite service demand",
                    phase.id
                );
                (if same { 1.0 } else { tc / sc }, label)
            }
        };
        durations.push(scale(
            plan.durations[i],
            factor
                * node_factor(
                    request.scaling,
                    phase.node_scaling,
                    request.source.nodes,
                    nodes,
                ),
        )?);
        bottlenecks.push(bottleneck);
    }
    Ok((durations, bottlenecks))
}
struct Schedule {
    starts: Vec<u64>,
    end: u64,
    critical: Vec<usize>,
    busy: BTreeMap<String, u128>,
}
fn schedule(
    request: &Request,
    target: &HardwareProfile,
    plan: &Plan,
    durations: &[u64],
) -> Result<Schedule> {
    let mut queues = slots(target);
    let mut ends = vec![0u64; durations.len()];
    let mut starts = ends.clone();
    let mut parents = vec![None; ends.len()];
    let mut busy = BTreeMap::<String, u128>::new();
    for &i in &plan.order {
        let mut ready = 0;
        let mut parent = None;
        for &dep in &plan.after[i] {
            if ends[dep] >= ready {
                ready = ends[dep];
                parent = Some(dep);
            }
        }
        let slot = if let Some(resource) = &request.workload.phases[i].resource {
            Some(
                queues
                    .get_mut(resource)
                    .with_context(|| format!("target lacks resource {resource}"))?
                    .pop()
                    .unwrap()
                    .0,
            )
        } else {
            None
        };
        if let Some((at, _, prior)) = slot {
            if at > ready {
                ready = at;
                parent = prior;
            }
        }
        starts[i] = ready
            .checked_add(plan.lag[i])
            .context("schedule start overflow")?;
        ends[i] = starts[i]
            .checked_add(durations[i])
            .context("schedule end overflow")?;
        parents[i] = parent;
        if let Some((_, slot, _)) = slot {
            let resource = request.workload.phases[i].resource.as_ref().unwrap();
            queues
                .get_mut(resource)
                .unwrap()
                .push(Reverse((ends[i], slot, Some(i))));
            *busy.entry(resource.clone()).or_default() += durations[i] as u128;
        }
    }
    let last = (0..ends.len()).max_by_key(|i| (ends[*i], *i)).unwrap();
    let end = ends[last]
        .checked_add(plan.tail)
        .context("schedule tail overflow")?;
    let mut critical = Vec::new();
    let mut current = Some(last);
    while let Some(i) = current {
        critical.push(i);
        current = parents[i];
    }
    critical.reverse();
    Ok(Schedule {
        starts,
        end,
        critical,
        busy,
    })
}
fn requirements(
    request: &Request,
    target: &HardwareProfile,
    nodes: u32,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut infeasible = Vec::new();
    let mut unsupported = Vec::new();
    for (name, required) in &request.workload.required_features {
        match target.resources.get(name) {
            None => unsupported.push(format!("No target profile for required resource {name}")),
            Some(resource) => {
                for f in required {
                    if !resource.features.contains(f) {
                        infeasible.push(format!("Target {name} lacks required feature {f}"));
                    }
                }
            }
        }
    }
    for (name, required) in &request.workload.memory {
        let base = integer(&required.bytes_per_source_node)? as u128;
        let bytes = if required.distributed && request.scaling == Scaling::Strong {
            (base * request.source.nodes as u128).div_ceil(nodes as u128)
        } else {
            base
        };
        if let Some(capacity) = target
            .resources
            .get(name)
            .and_then(|r| r.capacity_bytes_per_node.as_ref())
        {
            if bytes > integer(capacity)? as u128 {
                infeasible.push(format!(
                    "{name} requires {bytes} B/node, target declares {capacity} B/node"
                ));
            }
        } else {
            unsupported.push(format!("Capacity of target resource {name} is unknown"));
        }
    }
    Ok((infeasible, unsupported))
}

pub fn project(request: Request) -> Result<Projection> {
    ensure!(
        request.version == 1,
        "unsupported projection request version"
    );
    hash(&request.baseline.capture_sha256)?;
    integer(&request.baseline.duration_ns)?;
    integer(&request.baseline.dropped)?;
    integer(&request.baseline.omitted)?;
    ensure!(
        request.baseline.spans.len() <= 10000
            && request.baseline.metadata.len() <= 256
            && request.baseline.warnings.len() <= 10000,
        "baseline evidence limit exceeded"
    );
    for span in &request.baseline.spans {
        label(&span.id, 128)?;
        label(&span.name, 4096)?;
        label(&span.lane, 256)?;
        ensure!(
            span.attributes.len() <= 128,
            "span attribute limit exceeded"
        );
    }
    profile(&request.source)?;
    ensure!(
        !request.targets.is_empty() && request.targets.len() <= 32,
        "requires 1..=32 targets"
    );
    ensure!(
        request.nodes.len() <= 64 && request.targets.len() * request.nodes.len().max(1) <= 64,
        "at most 64 scenarios"
    );
    ensure!(
        request.workload.phases.len() * request.targets.len() * request.nodes.len().max(1) <= 16384,
        "projection exceeds 16384 phase/scenario combinations"
    );
    let mut ids = BTreeSet::new();
    for target in &request.targets {
        profile(target)?;
        ensure!(ids.insert(&target.id), "duplicate target ID {}", target.id);
    }
    let mut nodes_seen = BTreeSet::new();
    for &node in &request.nodes {
        ensure!(
            (1..=1_000_000).contains(&node) && nodes_seen.insert(node),
            "node counts must be unique and 1..=1000000"
        );
    }
    ensure!(
        request.validation.len() <= 64,
        "at most 64 validation observations"
    );
    for v in &request.validation {
        identifier(&v.target_id)?;
        ensure!(
            (1..=1_000_000).contains(&v.nodes),
            "validation node count must be 1..=1000000"
        );
        provenance(&v.provenance)?;
        ensure!(
            v.provenance.kind == ProvenanceKind::Measured,
            "validation observations must be measured"
        );
        ensure!(
            integer(&v.observed_ns)? > 0,
            "validation duration must be positive"
        );
        hash(&v.workload_sha256)?;
        hash(&v.target_profile_sha256)?;
    }
    let plan = plan(&request)?;
    let source_schedule = schedule(&request, &request.source, &plan, &plan.durations)?;
    ensure!(
        source_schedule.end == plan.window,
        "source reconstruction did not match selected window"
    );
    let mut hashes = BTreeMap::from([
        ("capture".into(), request.baseline.capture_sha256.clone()),
        ("evidence".into(), digest(&request.baseline)?),
        ("source".into(), digest(&request.source)?),
        ("workload".into(), digest(&request.workload)?),
    ]);
    let mut engine_hash = Sha256::new();
    for bytes in [
        include_bytes!("engine.rs").as_slice(),
        include_bytes!("model.rs").as_slice(),
        include_bytes!("lib.rs").as_slice(),
    ] {
        engine_hash.update(bytes);
    }
    hashes.insert(
        "engine_source".into(),
        format!("{:x}", engine_hash.finalize()),
    );
    let mut warnings = request.baseline.warnings.clone();
    warnings.push("Ranges propagate declared parameter bounds; they are not statistical confidence intervals.".into());
    warnings.push("The displayed range covers three evaluated fast/central/slow parameter cases; it does not bound every mixed parameter choice, scheduling anomaly, or modeling error.".into());
    warnings.push("The scheduler preserves execution-lane order and observed readiness lags; other causal structure must be supplied in the phase DAG.".into());
    warnings.push("Component cost ratios are anchored to observed phase durations; source reconstruction does not independently validate hardware parameters or service/wait separation.".into());
    if request
        .baseline
        .metadata
        .get("provenance")
        .is_some_and(|p| p == "synthetic")
    {
        warnings.push("The baseline is synthetic evidence, not a measured application run.".into());
    }
    if request.workload.memory.is_empty() {
        warnings.push(
            "No memory requirements were supplied; memory feasibility has not been established."
                .into(),
        );
    }
    if request.workload.required_features.is_empty() {
        warnings.push("No required ISA/device features were supplied; compatibility has not been established.".into());
    }
    let partial = !request.baseline.complete
        || integer(&request.baseline.dropped)? > 0
        || integer(&request.baseline.omitted)? > 0
        || plan
            .spans
            .iter()
            .any(|i| request.baseline.spans[*i].incomplete);
    if partial {
        warnings.push(
            "Partial/omitted evidence: parameter bounds do not account for unknown missing work."
                .into(),
        );
    }
    if request.baseline.clock_uncertainty_ns.is_some() {
        warnings.push("Baseline cross-host timing is estimated; model ranges do not include clock-alignment error.".into());
    }
    let mut assumptions=vec![request.workload.assumptions.clone(),"Observed per-phase release lags and trailing residual time are unchanged; resource scheduling uses a deterministic fixed list, not an optimal schedule.".into()];
    if plan.covered < plan.window && request.workload.unmodeled == Unmodeled::Unchanged {
        assumptions.push(request.workload.unmodeled_assumption.clone());
    }
    let mut scenarios = Vec::new();
    let referenced: BTreeSet<_> = request
        .workload
        .phases
        .iter()
        .filter_map(|p| p.resource.as_ref())
        .collect();
    for target in &request.targets {
        let target_hash = digest(target)?;
        hashes.insert(format!("target:{}", target.id), target_hash.clone());
        let counts = if request.nodes.is_empty() {
            vec![target.nodes]
        } else {
            request.nodes.clone()
        };
        for nodes in counts {
            let (infeasible, mut unsupported) = requirements(&request, target, nodes)?;
            if partial && !request.workload.allow_partial_evidence {
                unsupported.push("Incomplete, dropped, omitted, or unfinished evidence requires an explicit allow_partial_evidence assumption".into());
            }
            if plan.covered < plan.window && request.workload.unmodeled == Unmodeled::Reject {
                unsupported.push(format!("{} ns of the selected window has no modeled phase; specify an unchanged-time assumption or refine the window",plan.window-plan.covered));
            }
            if nodes != request.source.nodes {
                if request.scaling == Scaling::Fixed {
                    unsupported.push(
                        "Node count changes require an explicit strong/weak scaling model".into(),
                    );
                }
                if request.workload.scaling_assumption.trim().is_empty() {
                    unsupported.push("Node count changes require a scaling_assumption".into());
                }
            }
            if target.placement != request.source.placement
                && request.workload.placement_assumption.trim().is_empty()
            {
                unsupported.push(
                    "Placement changes require a placement_assumption for the fixed phase graph"
                        .into(),
                );
            }
            let mut s = Scenario {
                id: format!("{}:{nodes}", target.id),
                target_id: target.id.clone(),
                target_name: target.name.clone(),
                nodes,
                scaling: request.scaling,
                status: "estimated".into(),
                issues: Vec::new(),
                warnings: Vec::new(),
                runtime: None,
                speedup: None,
                efficiency: None,
                tasks: Vec::new(),
                resources: Vec::new(),
                critical_path: Vec::new(),
                validation: Vec::new(),
            };
            if nodes != request.source.nodes {
                s.warnings.push(format!(
                    "Representative-partition scaling assumption: {}",
                    request.workload.scaling_assumption
                ));
            }
            if target.placement != request.source.placement {
                s.warnings.push(format!(
                    "Placement assumption: {}",
                    request.workload.placement_assumption
                ));
            }
            let mut uncertain = BTreeSet::new();
            for resource in &referenced {
                for profile in [&request.source, target] {
                    if let Some(r) = profile.resources.get(*resource) {
                        for m in r.metrics.values() {
                            if matches!(
                                m.provenance.kind,
                                ProvenanceKind::SpecifiedPeak | ProvenanceKind::Assumed
                            ) {
                                uncertain.insert(format!("{} / {resource} includes assumed or specified-peak parameters; effective performance is conditional on the supplied phase assumptions",profile.id));
                            }
                        }
                    }
                }
            }
            s.warnings.extend(uncertain);
            if !infeasible.is_empty() {
                s.status = "infeasible".into();
                s.issues = infeasible;
                s.issues.extend(unsupported);
                scenarios.push(s);
                continue;
            }
            if !unsupported.is_empty() {
                s.status = "unsupported".into();
                s.issues = unsupported;
                scenarios.push(s);
                continue;
            }
            let calculate = || -> Result<_> {
                let (central, bottlenecks) =
                    durations(&request, target, nodes, &plan, Bound::Central)?;
                let (fast, _) = durations(&request, target, nodes, &plan, Bound::Fast)?;
                let (slow, _) = durations(&request, target, nodes, &plan, Bound::Slow)?;
                Ok((
                    schedule(&request, target, &plan, &central)?,
                    schedule(&request, target, &plan, &fast)?.end,
                    schedule(&request, target, &plan, &slow)?.end,
                    central,
                    bottlenecks,
                ))
            };
            let (scheduled, fast, slow, central, bottlenecks) = match calculate() {
                Ok(result) => result,
                Err(error) => {
                    s.status = "unsupported".into();
                    s.issues.push(format!("{error:#}"));
                    scenarios.push(s);
                    continue;
                }
            };
            // Changing durations can change resource-slot assignment. Include all
            // evaluated schedules in the envelope instead of promising monotonicity.
            let low = fast.min(scheduled.end).min(slow);
            let high = fast.max(scheduled.end).max(slow);
            s.runtime = Some(Estimate {
                optimistic_ns: low.to_string(),
                central_ns: scheduled.end.to_string(),
                pessimistic_ns: high.to_string(),
            });
            if scheduled.end > 0 {
                let speedup = plan.window as f64 / scheduled.end as f64;
                s.speedup = Some(format!("{speedup:.6}"));
                s.efficiency = match request.scaling {
                    Scaling::Fixed => None,
                    Scaling::Strong => Some(format!(
                        "{:.6}",
                        speedup * request.source.nodes as f64 / nodes as f64
                    )),
                    Scaling::Weak => Some(format!("{speedup:.6}")),
                };
            }
            let critical: BTreeSet<_> = scheduled.critical.iter().copied().collect();
            s.critical_path = scheduled
                .critical
                .iter()
                .map(|i| request.workload.phases[*i].id.clone())
                .collect();
            for (i, p) in request.workload.phases.iter().enumerate() {
                let model = match &p.model {
                    PhaseModel::Unchanged => "unchanged",
                    PhaseModel::Throughput { .. } => "throughput",
                    PhaseModel::Transfer { .. } => "latency_bandwidth",
                    PhaseModel::Roofline { .. } => "roofline",
                    PhaseModel::Unsupported { .. } => unreachable!(),
                };
                s.tasks.push(ProjectedTask {
                    id: p.id.clone(),
                    span_id: p.span_id.clone(),
                    name: p.name.clone(),
                    resource: p.resource.clone(),
                    model: model.into(),
                    assumption: p.assumption.clone(),
                    bottleneck: bottlenecks[i].clone(),
                    baseline_start_ns: plan.starts[i].to_string(),
                    baseline_duration_ns: plan.durations[i].to_string(),
                    start_ns: scheduled.starts[i].to_string(),
                    duration_ns: central[i].to_string(),
                    preserved_lag_ns: plan.lag[i].to_string(),
                    critical: critical.contains(&i),
                });
            }
            for (resource, busy) in &scheduled.busy {
                let slots = target.resources[resource].slots_per_node;
                s.resources.push(ResourceLoad {
                    resource: resource.clone(),
                    slots,
                    busy_ns: busy.to_string(),
                    modeled_busy_percent: if scheduled.end == 0 {
                        "0.000000".into()
                    } else {
                        format!(
                            "{:.6}",
                            *busy as f64 / (scheduled.end as f64 * slots as f64) * 100.0
                        )
                    },
                });
            }
            for v in request
                .validation
                .iter()
                .filter(|v| v.target_id == target.id && v.nodes == nodes)
            {
                if v.workload_sha256 != hashes["workload"] || v.target_profile_sha256 != target_hash
                {
                    s.warnings.push("Held-out observation does not match the current workload/target hashes; error was not computed.".into());
                    continue;
                }
                let observed = integer(&v.observed_ns)?;
                s.validation.push(ValidationResult {
                    observed_ns: v.observed_ns.clone(),
                    signed_error_percent: format!(
                        "{:.6}",
                        (scheduled.end as f64 / observed as f64 - 1.0) * 100.0
                    ),
                    provenance: v.provenance.clone(),
                });
            }
            scenarios.push(s);
        }
    }
    Ok(Projection {
        kind: "rprof-projection".into(),
        version: 1,
        engine_version: ENGINE_VERSION.into(),
        hashes,
        baseline_runtime_ns: plan.window.to_string(),
        covered_ns: plan.covered.to_string(),
        coverage_percent: format!("{:.6}", plan.covered as f64 / plan.window as f64 * 100.0),
        source_reconstructed_ns: source_schedule.end.to_string(),
        assumptions,
        warnings,
        request,
        scenarios,
    })
}
