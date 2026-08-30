# MAG optimization benchmark

`just bench-mag --output <report.json>` records one benchmark report. Reports include
the commit/tree identity and any tracked dirty diff digest, complete workload identity,
semantic oracle observations, wall-clock samples, and logical counters.

Comparison has two explicit modes:

```sh
# Informational comparison: always exits successfully after writing the verdict.
just bench-mag --baseline baseline.json \
  --comparison-output comparison.json --output candidate.json

# Acceptance gate: writes both artifacts, then exits 2 when any required gate fails.
just bench-mag --baseline baseline.json \
  --comparison-output gate.json --output candidate.json --gate
```

Gate mode requires a clean candidate tree. It rejects workload-definition or
benchmark-environment mismatches before treating performance results as comparable,
and independently requires semantic equivalence, the selected target's median ratio
to be at most `0.90`, its selected logical counter reduction to be at least `40%`,
and every timed case's p90 ratio to be at most `1.10`. The comparison artifact
lists every case that violates the p90 gate.

Module roots have stable labels and explicit workload or implementation roles. Root
order, labels, and roles are fingerprinted. Workload-root `.mag` contents contribute
to workload identity; implementation-root contents do not, so a library candidate can
be compared with the baseline whose workload it implements. Absolute root paths never
contribute to the workload hash.

Distributions use the empirical **nearest-rank** quantile: for percentile `p` and
`n` sorted observations, select one-based rank `ceil(p × n)`. This conservative
tail policy makes p90 the maximum observation when `n = 3`; with the default
`n = 30`, p90 is observation 27. Reports persist this policy alongside their
sample count so incompatible statistics cannot be compared.
