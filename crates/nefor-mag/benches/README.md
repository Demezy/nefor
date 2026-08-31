# MAG optimization benchmark authority

The A0 benchmark uses two persistent worker processes. A same-version optimization gate is valid only when the baseline worker is built from the exact clean A0 commit and the candidate worker is a distinct executable built from the exact clean candidate commit. Historical reports from `70cd1a8` and `acaec5f3…`, and independent marginal reports produced with `--baseline`, are diagnostic only.

```sh
just bench-mag --paired \
  --baseline-worker /path/to/a0/mag_compile \
  --baseline-root /path/to/clean/a0/checkout \
  --candidate-worker /path/to/candidate/mag_compile \
  --candidate-root /path/to/clean/candidate/checkout \
  --target-case broad-frontier-lower-16x8 \
  --target-counter evaluator_steps \
  --output tmp/gate.json --gate
```

The coordinator verifies both clean commit/tree identities, both executable SHA-256 digests, the benchmark-definition identity, workload and fixture fingerprints, policy versions, and the persistent-worker protocol before interpreting timings. Worker startup, fixture construction, semantic observations, logical-counter profiling, and warmup happen before timed requests. Each case then uses a baseline-calibrated batch count frozen for both workers and adjacent deterministic balanced `AB/BA` blocks. A crash, malformed response, missing case, changed identity, response-order mismatch, or exact semantic mismatch fails closed.

A self-comparison is available only for calibration:

```sh
just bench-mag --paired --calibration --output tmp/a0-self-calibration.json
```

When endpoint arguments are omitted in calibration mode, the coordinator spawns its current executable twice against the current clean source root. The report is always labeled `calibration_only_non_authoritative`; it can never create an optimization pass. Identical executable or source identities are rejected outside calibration with the statement that identical endpoints are calibration-only and cannot satisfy an optimization gate.

For adjacent paired batch observations, the report analyzes
`log(candidate_batch_ns / baseline_batch_ns)`. Median and p90 use the empirical nearest-rank statistic. Confidence bounds bootstrap the nearest-rank **p90 itself** with 131,072 deterministic paired resamples. The fixed mandatory case family receives a per-tail error allocation of `0.05 / (2 × case_count)`. An upper bound at most `1.10` passes, a lower bound above `1.10` is a regression, and every other result is inconclusive. The explicitly selected target must also have a paired median ratio at most `0.90` and reduce its selected generic logical counter by at least 40%. Only a pass may advance an optimization; inconclusive gates follow the preregistered `30 → 60 → 120` schedule and remain failed if unresolved.

Stage evidence retains workload, fixture/source, topology, forced-terminal, direct-prerequisite, and expected semantic/artifact fingerprints. Counter subtraction is `exclusive` only when the complete boundary matches, direct forcing is proved, and every counter delta is nonnegative. Lowering is never labeled exclusive: it is **inclusive after a proven analysis prefix** only when the implementation still exposes that prefix, and otherwise remains plainly inclusive. Broad lowering also retains the full untimed lowered artifact observation.

`legacy_cycle2_manifest.json` remains the immutable pre-cycle-3 historical oracle. `current_main_a0_manifest.json` records the exact inherited fixture and artifact identities after the experiment is semantically rebased over current main. A current-main change to config-owned MAG sources advances the current A0 workload and oracle fingerprints without rewriting that historical manifest; inherited case/oracle membership, order, and the 25-oracle policy remain fixed.

The report/statistics policy is fail-closed. Reports with the previous schema, median-bootstrap bounds, absent worker-pair identity, or old exclusive-section evidence are incompatible and remain diagnostic evidence only.
