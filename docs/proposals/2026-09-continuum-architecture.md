# MirageSSD Continuum v2: compiled, budgeted game-data readiness

Status: research proposal, not an implemented replacement architecture or a game-compatibility certification.
Research reviewed: 2026-09-14. Baseline: the working tree following `results-perf-ordered-1`.
This revision supersedes the first Continuum proposal in this file, including its unsupported claims that seventeen bottlenecks had deterministic solutions.

## Executive decision

The strongest architecture is not a universal cloud drive with a better predictor. It is a **game-data readiness compiler and runtime**:

1. Bind an exact game build and configuration to authenticated content and recovery sources.
2. Compile a placement plan using the capabilities of each presentation backend, not a preferred filesystem brand.
3. Admit a workload only after checking both its space requirements and its data-arrival requirements.
4. Make uncertainty explicit. Learned predictions improve preparation; they do not prove that arbitrary future gameplay is covered.
5. Use publisher-controlled transition barriers when a genuine complete dependency contract is available.
6. Maintain one custody, scheduling, versioning, and diagnostics model across native files, WinFsp, and any qualified Windows-native projection.

**Keep the measured WinFsp implementation as the baseline. Do not replace it with CFAPI on the assumption that NTFS placeholders imply native speed or a hard byte-range cache budget.** CFAPI and ProjFS are competing candidates that must pass the same experiments. A publisher-integrated asset interface is a separate, potentially stronger route; it is not something we can inject into an unmodified game.

This design addresses all twenty identified bottlenecks. It does not establish a universal combination of tiny storage, unlimited offline exploration, zero latency, and third-party approval. Where those requirements conflict, the compiler must report that no qualified plan exists rather than advertise success.

## Evidence and confidence

- **Measured:** existing synthetic experiments, not a general property of games.
- **API fact:** a documented platform contract, with its conditions intact.
- **Design:** a proposed mechanism with a specified proof obligation.
- **Experiment required:** behavior or performance must be demonstrated on the actual platform and title.
- **External decision:** publisher permission, legal interpretation, signing eligibility, or customer adoption.

The measured baseline remains useful:

| Property | Recorded result | What it establishes |
|---|---:|---|
| Matched warm workload, median mean | Mirage 131.6 us; native NTFS 139.2 us | Native-class cached reads on this workload |
| Matched warm workload, median p99 | Mirage 324.9 us; native 357.9 us | This measured latency distribution, not a frame-time guarantee |
| Repeated unbuffered 1 MiB reads | Mirage 1,296.9 us; native 785.6 us | A remaining approximately 65% mounted-read penalty |
| Capsule materialization | 19.93 s baseline; 9.27 s ordered | Preparation improved, but is not instantaneous |
| Arena physical extents | 1,055 before locality work; 95 afterwards | A real layout improvement, not a fragmentation-free filesystem |
| Profiled offline workload | 4,064 successful reads; zero byte mismatches | The known workload can run from its prepared set |
| Held-out offline workload | 2,895 failed reads out of 4,064 | Training-set residency does not imply future coverage |
| Eight-session synthetic Zipf capsule | 3.29 GB of 4.29 GB; 524 held-out read failures | More training can consume most storage without eliminating misses |

See the preserved experiment report and raw results under `D:\MirageExperiment`. These results are not Genshin gameplay measurements. The locality changes are in the working tree; this document does not imply a commit, merge, or release.

Read-only platform checks performed during this research:

- Windows 11 Home, OS build 26200; the experiment volume is NTFS.
- `CfGetPlatformInfo` returned success, build 26100, revision 9278, integration number `0x628`.
- `Client-ProjFS` returned `InstallState=2`, meaning disabled.
- No sync root was registered, no placeholder was created or dehydrated, and no Windows feature or security policy was changed.

API presence is not proof of partial-hydration behavior, performance, compatibility, or support for a particular title. [S01], [S02]

## Corrections to the first proposal

| Earlier claim | Corrected conclusion |
|---|---|
| CFAPI removes the uncached-read gap by construction | It can avoid the provider callback for valid locally available data, but the filter stack, file state, buffering, and device still matter. Measure it. |
| Range hydration means a strict range-sized local footprint | `PARTIAL` is documented as unsupported; `PROGRESSIVE` can continue hydration while handles remain open. A range API does not bound total allocation. |
| Dehydrating after play enforces the budget | That controls eventual usage, not peak usage during play. The budget must hold at every admitted allocation. |
| `CfSetPinState` proves residency | Pin state is intent; successful pinning does not guarantee hydration has completed. Pins are file-level, not arbitrary range leases. |
| Changing file identity automatically removes stale data | `CfUpdatePlaceholder` has an explicit invalidation range array and exclusivity requirements. Multi-file generation changes still need a protocol. |
| ProcessInfo or PID 4 separates all demand and read-ahead | These fields do not establish semantic demand or causal attribution. Keep an unknown category and correlate application completion separately. |
| Tracing every non-game read proves a permanent native boundary | A finite trace cannot prove future access. Treating every AV scan as permanent native placement can also eliminate all storage savings. |
| A quiet test account for fourteen days proves compatibility | It does not. Do not use disposable-account penalties or intentional missing-data failures as a compatibility certification. |
| Bao makes any publisher 64 KiB read cost 64 KiB | Only when trusted proofs and a suitably random-access representation exist. Compression and encryption boundaries still constrain transfer and decoding. |
| A 16 KiB outboard necessarily costs at most 0.2% | A binary outboard storing two 32-byte child chaining values per retained internal node approaches 0.390625% at 16 KiB groups, before other metadata. |
| Streaming never stalls when average consumption is below bandwidth | Burst size, lead time, queueing, outages, correct identity, and decode/commit time also matter. |
| PresentMon identifies loading screens and game regions | It measures presentation timing. A loading-state classifier derived from it is a hypothesis, not an authoritative game signal. |
| Updating only resident files bounds all patch dependencies | A binary delta can require nonresident old bytes. Reference-data dependencies and rollback costs must be included. |
| In-place conversion makes preparation metadata-only | Trust establishment, hashing, source verification, dehydration, allocation changes, and rollback can dominate preparation. |
| Inbox components or user-owned copies guarantee acceptance and rights | Neither is a blanket compatibility or legal authorization. |
| EV signing is required for user-mode releases | Valid Authenticode signing is needed; the appropriate certificate/service depends on distribution and eligibility. EV does not guarantee SmartScreen reputation. |

ADR 0001's blanket full-hydration rationale should be revisited using the documented range APIs. Its prohibition on injection, anti-cheat bypass, hidden drivers, and fabricated integrity results remains in force. Correcting the rationale does not establish that CFAPI is the best replacement. [S03], [S04], [S05], [S06], [S07], [S23]

## 1. Feasibility before optimization

### 1.1 Three different promises

| Mode | Required evidence | Honest promise |
|---|---|---|
| Verified scope | Complete, authenticated dependency closure for a declared scope; verified residency; protected lifetimes | No origin fetch is needed for those declared immutable bytes while the contract holds |
| Profiled/adaptive play | Version-bound observations, a tested cache plan, and an available origin | Qualified best-effort streaming with measured miss and stall rates; not unlimited offline coverage |
| Full-local compatibility | All required files materialized and verified on native storage | Native file presentation, at the cost of its actual local footprint |
| Maintenance | Explicit update/verify/restore operation and its own reservations | The operation may consume time, bandwidth, and additional storage, all reported separately |
| Unsupported under this budget | No plan meets compatibility, space, and delivery constraints | Refuse to claim ready; preserve originals and explain which constraint failed |

A learned capsule may be completely resident without being a complete dependency closure. Call that a **pinned prepared set**, not proof of arbitrary future gameplay. Full-local fallback is useful, but is not success on the small-SSD requirement if it exceeds the chosen budget.

### 1.2 The offline limit

Let `U` be the content that could be required within the promised scope and `L` the locally recoverable content. With the origin unavailable, a guarantee for every allowed execution requires `U` to be contained in `L`.

If an unmodified game can request any asset in its install and we have neither a complete dependency contract nor control over transitions, a training trace does not shrink `U` with certainty. Better prediction can lower empirical misses; it cannot turn an unobserved dependency into locally available bytes.

The constructive solution is to narrow and enforce the scope through an authorized game integration, or to offer qualified online behavior. Do not hide the distinction behind a cache-hit percentage.

### 1.3 The temporal admission test

For every required unit `i`, readiness requires:

`verified_available_time(i) <= required_time(i)`

A practical latest-start estimate includes queue delay, source time-to-first-byte, actual encoded transfer bytes divided by effective goodput, decoding, verification, placement, and a measured safety margin. Predictors must supply lead time, not just a popularity rank.

Illustrative lower bounds, calculated in this research, assuming 50 Mb/s payload throughput and one 40 ms request RTT before transfer:

| Missing transfer | Transfer plus RTT, before server queueing, decoding, or disk work |
|---|---:|
| 1 MiB | 207.77 ms |
| 64 KiB | 50.49 ms |

These are not measured promises about a provider. They show why a critical read with only one 16.7 ms frame of notice cannot rely on such a WAN miss, even when long-run bandwidth is ample.

Network calculus offers a useful model: an arrival envelope bounds bursts; a service curve bounds delivered work. A rate-latency service curve `beta(t)=r*max(0,t-T)` is useful only when its lower bound is justified. Public internet measurements provide a statistical forecast, not a deterministic lower service bound during arbitrary outages. Aggregate byte inequalities are necessary checks, not sufficient proof that the correct assets arrived. [S15]

### 1.4 The spatial admission test

For Mirage-managed storage, enforce at all times:

`allocated + reserved_new_allocation + dirty_staging + rollback_retention + journal_and_metadata + filesystem_slack <= B_managed`

The categories must not double-count the same reservation, and must not omit simultaneous copies. A retained arena copy and an NTFS hydrated copy are two allocations even if their content hash is identical. Keep a separate RAM credit pool and a separate measured total-game-footprint view.

Important boundaries:

- A sparse logical file length is not physical allocation.
- A database reservation does not reserve free disk space against unrelated applications. Use an appropriate physical reservation strategy where a hard promise requires it, or handle allocation failure without falsely acknowledging data.
- The game and Windows can write outside Mirage's controlled storage. Report that footprint and preserve free-space margins; do not claim to cap arbitrary external writes.
- A projection backend with unavoidable whole-file hydration must reserve that allocation closure, or must not admit the file under the selected budget.
- Checking usage only after eviction or after a session is not a hard budget test.

## 2. The unified system

### 2.1 Compile data readiness, not a drive letter

```
Authenticated content/version catalog + title compatibility policy
                         |
          Readiness and placement compiler
   (dependency closure / predictions / capability inventory)
                         |
            Spatial + temporal admission
                         |
             Lifecycle coordinator
                         |
         Shared fetch, verification, custody core
               /         |          \
       native files    WinFsp    qualified CFAPI/ProjFS
                         |
        Application outcomes + I/O telemetry + frame timing
```

A publisher-integrated asset interface can use the same core without filesystem interception. An optional LAN block-storage investigation belongs behind a separate capability gate, not inside the first consumer implementation.

The compiler emits a **readiness record**, not a claim of certainty obtained from a model. It binds:

- Game/build identity, manifest digest, configuration, language/content selection, and profile schema.
- Scope identifier and whether its dependency closure is complete or empirical.
- Exact required verification units and native-file requirements.
- Presentation choice and its observed hydration/eviction capabilities for each file or supported subtree.
- Trusted content roots, lengths, recovery sources, and authorization policy.
- Physical reservation plan, update/rollback obligations, and RAM/in-flight limits.
- Backend, OS, driver, and runtime qualification versions.
- Pin/lease generation and the conditions that invalidate readiness.

A signed record authenticates its issuer and contents. It does not prove that a heuristic model predicts every future read or that an internet connection cannot fail.

### 2.2 Capability-selected presentation

Presentation is normally selected per file or supported subtree. Do not pretend an arbitrary unmodified `.pak` can use native NTFS for its hot prefix and another filesystem for its remaining offsets without a platform mechanism that actually supports that behavior.

| Backend | Strength | Constraint that the compiler must price | Adoption rule |
|---|---|---|---|
| N: complete native files | Existing native semantics; suitable for executables, mutable state, small files, and expensive hot files | Full retained file bytes; update and rollback obligations | Default for compatibility-sensitive or unclassified data, provided the total plan fits |
| V: WinFsp immutable projection | Current measured implementation; explicit verified-page residency and controlled misses | Existing large-uncached-read overhead; shared-state and cancellation work still required | Baseline for experiments and qualified titles; do not discard before a replacement wins |
| H: Cloud Files API | Inbox Windows filter; documented range transfers and local hydration | Progressive background growth, file-level pin intent, exclusive dehydration, service/policy interactions, filter overhead | Candidate only after allocation-closure, latency, compatibility, and recovery gates pass |
| P: ProjFS | Windows-backed projected namespace and data callbacks; documented cache-state/version mechanisms | Optional component; requested range may exceed the app read; no presumed range eviction or WAN behavior | Candidate especially for fast local/LAN origins; measure actual granularity and semantics |
| D: authorized game asset interface | Explicit dependencies and transition barriers; can avoid the filesystem bridge | Requires game/publisher integration; does not retrofit unmodified Genshin | Strongest route to complete scoped readiness guarantees |

CFAPI and ProjFS are not automatically interchangeable with a page-addressed arena. Their capability descriptors must include worst-case hydration growth, eviction granularity, restrictions on open/mapped files, mutation behavior, cancellation, and persistent-state reconciliation. [S03], [S04], [S05], [S06], [S08], [S09], [S10], [S11]

A useful experiment compares all viable candidates on the same bytes and access trace. It must reject a superficially faster backend that saves less storage than the product requirement or fails real I/O semantics. Microsoft's ProjFS overview specifically describes high-speed backing stores and points slow-source scenarios toward Cloud Files API; neither direction is a game-performance certification. [S30]

### 2.3 Publisher-controlled transitions: the strongest new connection

An authorized integration exposes a transition such as `PrepareScope(build, scope, configuration)` before the game enters the corresponding content. The game supplies a complete dependency closure or a publisher-authored content-group map. Mirage fetches, verifies, reserves, and pins that closure. The game resumes the transition only after readiness is acknowledged.

This combines compiler-style dependency analysis, real-time admission, and storage leases. It moves uncertainty to a controlled loading boundary rather than into a render-critical read. The scope includes shared dependencies and dynamic content rules; a region name alone is not a dependency proof.

There is no unauthorized injection or automatic pausing of a protected process. For an unmodified title, user intent and observed transitions can improve prefetching, but remain hints. Microsoft's streaming-install/content-group model is a precedent for explicit cooperation, not proof that crowd traces are equivalent to a developer dependency map. [S24]

### 2.4 Separate identity, verification, transfer, and placement

Four different units are required:

1. **Logical identity:** immutable game file/build identity and length.
2. **Verification unit:** independently verifiable original bytes.
3. **Transfer unit:** what the authorized origin can actually deliver and decode independently.
4. **Placement unit:** what the local backend allocates, pins, and can reclaim.

First measure the already supported 64 KiB, 256 KiB, and 1 MiB page geometries on fresh fixtures. Changing page size must not reinterpret an existing arena in place. Smaller pages can improve miss amplification but increase index size and I/O count; coalescing and read leases must be evaluated together.

For a future format, use either signed range-hash metadata or a correctly implemented Bao-compatible outboard with a trusted BLAKE3 root and authenticated length. A flat list of independent leaf hashes is not automatically the BLAKE3 hash tree of the whole file. Bao decoders also have specific final-chunk/EOF validation requirements. [S12]

**Trust bootstrap is a separate operation.** A hostile source can hash its own wrong bytes perfectly. Bootstrap expected identity from an authenticated publisher manifest/integration, or an explicitly trusted local import with recorded provenance. Generate an outboard by reading the required trusted representation; do not claim that publisher MD5 metadata alone supplies a BLAKE3 root or sub-range proofs. Protect Mirage's software, manifests, and coverage-pack distribution with TUF-style role separation, version/snapshot consistency, expiration policy, and key rotation. TUF protects the update process but explicitly does not bootstrap trust in arbitrary new software or eliminate denial of service. Valid bytes from an already trusted pinned generation can remain usable offline under an explicit offline policy; that must not become silent acceptance of replayed update metadata. [S29]

For Mirage-controlled archives, independent compressed frames and, where encryption is enabled, independently authenticated frames can align with verification units. Preserve the original game bytes at the presentation boundary. A larger placement extent may contain smaller verified units if a versioned suballocation format justifies the complexity.

For publisher-owned compressed chunks, obey the publisher's actual encoding. A proof for 64 KiB of plaintext cannot make the middle of an ordinary zstd frame independently decompressible, nor authenticate only part of a larger AEAD message. Fetch/decode the required independent frame unless an authenticated seekable representation exists. Zstandard's seekable format explicitly adds independent frames and a seek table; that structure cannot be assumed for an arbitrary Sophon chunk. [S13], [S14]

Outboard sizing must be measured. For a binary outboard retaining two 32-byte child values per internal node, the asymptotic tree storage is approximately `64/group_bytes`: 0.390625% at 16 KiB groups or 0.09765625% at 64 KiB groups, before manifests, indexes, framing, and signatures. Wire proof overhead depends on the requested ranges and shared tree paths.

### 2.5 One bounded fetch path

Extend the existing `ObjectBackend`, `PageProvider::get_or_fetch`, `FlightMap`, scheduler queues, and verification worker rather than creating parallel origin/fetch systems that duplicate them.

The proposed request lifecycle is:

`admit identity and authorization -> join/create flight -> reserve owner resources -> fetch bounded representation -> decode/authenticate -> verify original bytes -> place durably -> publish matching generation -> complete subscribers`

Concrete requirements:

- Share work only inside the appropriate authorization/trust domain. A public hash is not permission to disclose content across users or tenants.
- Join a flight before charging a duplicate physical download reservation. Keep separate bounded credits for subscribers and their output buffers. The current provider reserves before joining its flight, which deserves explicit duplicate-request tests.
- Use awaitable completion and a bounded worker pool. Do not hold namespace locks or block the only callback/executor thread while a fetch waits for work that needs that same thread.
- Give demand, admission, maintenance, and speculative work distinct policies. Application deadlines are not the CFAPI cancellation timeout. Preserve fairness and prevent speculative work from consuming demand reserves.
- Coalesce only within the same immutable representation and useful deadline window. HTTP range support is not guaranteed; validate status, content range, representation identity, encoding, and bounded length. A full `200` response to a small range must not silently exhaust memory or disk. [S16]
- Cancellation detaches a subscriber. The underlying fetch is cancelled only when no remaining subscriber or admitted preparation needs it.
- Recheck generation/request identity before placement and publication. A late completion from an old build must never populate a new build's mapping.
- Treat checksum failure, authorization failure, source disappearance, timeout, budget exhaustion, and caller cancellation as different causes. Never substitute zeros, false EOF, or fake success.

### 2.6 One custody model, with real reader protection

Use a publication state machine:

`Absent -> Reserved -> Fetching -> VerifiedStaging -> Writing -> DurableVerified -> Published`

Failure before publication leaves no readable resident mapping. Keep the existing write/flush/read-back verification/metadata/transaction discipline for the arena. Backend-specific placement needs its own tested durability boundary; a success callback is not assumed to prove every later layer is durable.

A shared bitmap or sequence counter alone does **not** prevent a slot from being reused after a reader has checked its generation. The final arena protocol must couple mapping generation with exclusion:

1. A reader obtains a lease for the mapped content and slot epoch before accessing bytes.
2. The writer can retire/reuse the slot only after all applicable pins and reader leases are gone.
3. Publishing a reused slot increments its epoch and uses release/acquire ordering.
4. A reader revalidates the mapping after acquiring the lease and retries if it changed.
5. A crashed reader's resources are reclaimed only after its owning process/job is confirmed dead, not merely because a heartbeat was late.

Start with the simpler safe restriction that published slots of a sealed mounted generation cannot be reused. Live adaptive eviction requires a separately qualified cross-process lease mechanism or a design in which all relevant readers and writers share one proven ownership domain. In-process atomics in two independently built `ResidentIndex` instances are not automatically cross-process synchronization.

CFAPI pin intent is file-level and asynchronously hydrated. It cannot stand in for this range lease. ProjFS dirty/full files must not be deleted as if they were disposable cache. [S05], [S09], [S10]

### 2.7 Prediction is a performance advisor, not the trust authority

Use a progression of baselines: access-order prefetch, recency/frequency with scan resistance, co-access clusters, then a confidence-aware next-context model. Compare against a simple policy at equal disk, RAM, network, and CPU budgets. The repository already has frequency, ghost-history, admission, and recency machinery; do not credit those components to a nonexistent implementation in another crate. ARC and TinyLFU provide relevant research precedents, not universal predictions of this workload. [S17], [S18]

Optimize expected avoided stall cost per physical byte, taking actual source amplification and backend allocation closure into account. A frequently accessed cheap page is not always more valuable than an infrequent, expensive, deadline-critical page. Keep mandatory pins outside the replacement competition.

Prefix/phase classification must have an abstention state. A new region can need its first unknown asset before the classifier has enough evidence. PresentMon timing and I/O bursts can be inputs, but not authoritative loading-screen or region labels.

Cloud profile sharing is optional. Hashes and timing sequences are not anonymous: known asset hashes can reveal gameplay context. Start local-first, obtain explicit consent for sharing, bound contributions and retention, and separate prediction poisoning defenses from content-integrity checks. Any differential-privacy claim needs a defined privacy unit, contribution bounds, epsilon/delta, and composition accounting; adding noise to a Count-Min sketch is not sufficient. [S19]

### 2.8 Treat updates like versioned deployments

Build an update dependency DAG from authenticated new content, reusable old content, patch reference requirements, and rollback obligations. A delta may need old bytes that are currently nonresident. If reference dependencies cannot be established, conservatively materialize the required reference file or use an authorized full replacement.

Do not mutate live readers into a new build. Prepare and verify a new generation, quiesce the affected game/update operations, activate a consistent view, then reclaim old data after reader and rollback retention end. Native-file changes require their own journaled replacement sequence; a database pointer alone cannot atomically replace a whole native directory tree.

Account for the peak union of retained old data, new data, patch inputs, encoded/decoded staging, snapshots, and metadata. Deduplicate identical immutable content where representation permits, but do not use ordinary hard links as copy-on-write isolation for mutable files.

The official launcher continues to own its verification and installed-version state in the cooperative mode. An alternative launcher requires an explicitly supported integration and legal review; it must not fake the official launcher's completion records.

## 3. Resolution of every inventoried bottleneck

Each acceptance condition below is a proposed gate, not a test result already achieved.

### B01 — Real Genshin and anti-cheat compatibility

**Resolution:** a versioned compatibility program, not a reparse-tag assumption. Keep execution, anti-cheat, and mutable components native. Qualify asset projection separately, using permitted observations and intact software.

**Implementation:** store a compatibility record keyed by title/build, backend, OS/driver versions, and configuration. Test synthetic and owned/non-anti-cheat titles first, then use an authorized publisher/test environment for protected titles. Use the official launch path unless another path is expressly supported. No injection, anti-cheat bypass, concealed metadata, security-policy removal, or intentional missing-data fault tests against protected public accounts.

**Acceptance:** normal launch, play, exit, verification, and update succeed for the declared matrix; all assets are byte-correct; no new compatibility failures appear. Record the scope of the evidence and any publisher approval separately.

**Failure behavior:** disable that projection for that title/version and preserve a recoverable native mode. If native mode exceeds the budget, report unsupported under that budget. No finite account-observation period proves perpetual acceptance. **Status: experiment required and external decision.**

### B02 — Prediction does not cover unseen gameplay

**Resolution:** separate complete scoped dependency contracts from empirical prediction. The strongest route is the authorized `PrepareScope` barrier; the unmodified-game route remains profile-driven online preparation.

**Implementation:** combine version-bound local traces, optional vetted aggregate profiles, confidence-aware clustering, access-order prefetch, and explicit user/launcher intent where available. Use time- and player-disjoint evaluation, including first visits, long sessions, language changes, unusual routes, and new content. A publisher chunk manifest describes storage layout, not automatically scene dependencies.

**Acceptance:** zero origin requests for executions inside a genuinely complete, pinned scope with the origin cut; for adaptive play, report prefetch misses, I/O failures, stall distribution, overfetch, and confidence separately. Reject the old gate allowing 1% application read failures.

**Failure behavior:** abstain from an offline guarantee when scope completeness is unknown. A missing required byte still fails honestly or is fetched within the online contract. **Status: design plus empirical qualification; not solved by collecting more sessions alone.**

### B03 — Coverage grows the local footprint

**Resolution:** compile the closure of required content and backend-induced allocation, then choose a feasible point on a measured storage/latency/coverage frontier.

**Implementation:** pin the complete current scope when available; reserve transitions before admitting them; evict only eligible data. Price CFAPI progressive files by their possible full-file hydration and pin behavior, not just the range initially requested. Use WinFsp where strict page placement is necessary and qualified. Between-session dehydration is useful housekeeping but not the enforcement mechanism for peak limits.

**Acceptance:** under a synthetic traversal, held handles, memory mapping, and background reads, every managed allocation fits the declared envelope at every sample and audited transition. When a requested scope's non-evictable closure exceeds the budget, admission refuses before making an unsupported promise.

**Failure behavior:** larger budget, a different qualified backend/scope, or unsupported status. Arbitrary offline exploration cannot be obtained from a smaller unproven resident set. **Status: design, constrained by measured working sets and platform behavior.**

### B04 — Incomplete online miss pipeline

**Resolution:** connect the real filesystem boundary to the existing shared scheduling and verification core.

**Implementation:** extend `mirage-backend`, `mirage-scheduler`, and `mirage-engine` rather than create duplicate fetch stacks. Bridge WinFsp pending reads or a qualified platform callback to bounded asynchronous work. Keep read-only publisher adapters distinct from archive write/delete capabilities. Drive, local, and any approved HTTP/publisher adapter must advertise representation and recovery capabilities explicitly.

**Acceptance:** a held-out workload completes byte-correctly over controlled network profiles; duplicate subscribers share work; cancellation does not starve surviving readers; malformed ranges, truncated bodies, and source revisions are rejected; no event-loop or namespace-lock deadlock occurs.

**Failure behavior:** a typed error when the origin cannot satisfy the request, and an explicit readiness downgrade. The CFAPI 60-second cancellation mechanism is not a gameplay latency target. **Status: implementation work; supporting components already exist.**

### B05 — No complete live durable adaptation

**Resolution:** verified miss data becomes a budgeted durable resident through the custody state machine, with safe publication to live readers.

**Implementation:** replace one-shot host residency assumptions with a qualified update/lease protocol. Use cross-process generation-and-lease protection for live eviction, or disallow slot reuse while a mounted sealed generation can read it. Give CFAPI/ProjFS their own reconciliation adapters instead of equating hydrated bytes with verified bytes.

**Acceptance:** concurrent demand, promotion, eviction, cancellation, host death, and service restart produce no mismatched bytes or use-after-reuse; a live host sees new residents without a remount where that capability is promised. One hundred concurrent readers for the same unit consume one fetch/placement reservation, plus bounded subscriber resources.

**Failure behavior:** stop reuse, retain pins, or require a controlled remount if coordination is unhealthy. A heartbeat timeout alone is not permission to reclaim a live reader's slot. **Status: design and concurrency qualification.**

### B06 — Native versus virtual classification

**Resolution:** explicit title policies plus positive observations; never infer permanent safety from the absence of a write in a short trace.

**Implementation:** native placement for executable images, anti-cheat components, mutable data, and unsupported operations. Use manifests, path/file-class rules, permitted ETW observations, and representative lifecycle runs to identify candidate immutable assets. Unknown classifications default conservatively, or fail the small-budget plan. Do not promote an entire game to native merely because an antivirus scanned it; instead ensure its reads are correctly served under a qualified presentation and account for the resulting hydration.

**Acceptance:** complete operation traces for install, launch, play, exit, repair, and update; no virtualized file receives an unsupported mutation; synthetic scanners and service-like readers receive correct bytes under the tested platform policies.

**Failure behavior:** controlled reclassification only with the affected readers stopped and capacity available. No live hidden backend switch. **Status: title-specific qualification.**

### B07 — Patching and repair

**Resolution:** a maintenance transaction with a complete patch-input and rollback space plan.

**Implementation:** reuse `mirage-engine/update`, native snapshots, generation activation, and the service coordinator. Where authorized, adapt Sophon chunk/patch metadata without assuming that only resident old files are needed. For the official launcher, stage native data it actually requires and let it perform its real update/verification. For an approved custom integration, generate the new versioned view from authenticated content and reference data.

**Acceptance:** a real allowed version transition matches the native result for all relevant file bytes and metadata; injected failures on owned fixtures preserve a recoverable old or new generation; delayed old requests cannot modify the new view. Include full-file repair scans and unavailable old-version sources.

**Failure behavior:** defer maintenance when peak space or reference data cannot be secured. Never skip a required verification, forge launcher state, or delete rollback data early. **Status: implementation and publisher-workflow qualification.**

### B08 — Profile validity across versions

**Resolution:** separate reusable content statistics from version-specific claims about where and when content is required.

**Implementation:** enforce repository, manifest, schema, and configuration binding before resolving ordinals. Store observations against authenticated content identities and original byte ranges. Transfer statistics for unchanged content, but requalify scope membership and deadlines when code/configuration changes. Changed paths are hypotheses, not proof that old offsets still mean the same thing. New configuration/language data does not inherit a sealed guarantee by similarity.

**Acceptance:** tests for reordered files, shifted offsets, unchanged files, changed content at the same path, schema mismatch, wrong repository, changed language, and stale signed profile packs. Invalid bindings fail closed; unchanged identities transfer only the properties they actually preserve.

**Failure behavior:** require a fresh or explicitly remapped profile and withdraw readiness until it is verified. **Status: concrete implementation work.**

### B09 — Large uncached reads are slower than native

**Resolution:** an evidence-gated presentation choice and measured hot-path experiments; no assumption that one API eliminates the penalty.

**Implementation:** keep the current aligned direct-buffer path, since forcing large scratch copies was slower. Benchmark raw native files, the arena, FFI, WinFsp, CFAPI hydrated files, and ProjFS at matched modes and queue depths. Profile dispatch, copying, faults, allocation, and backing I/O separately. Test overlapped backing handles and bounded completion processing; merely passing an offset to a synchronous handle is not proof of asynchronous overlap. Coalesce adjacent resident ranges only while protecting every contributing slot's lifetime. Prefer whole native hot files when their full footprint fits. An authorized asset interface can remove the filesystem bridge, but requires a cooperating build. [S20]

**Acceptance:** proposed native-class gate: no byte errors and a qualified non-inferiority result for the declared warm and direct workloads, with explicit p99 targets and total memory/space budgets. Do not average away a failing request-size class.

**Failure behavior:** retain the best measured backend and report the remaining penalty. The current approximately 65% large-uncached gap is not closed by this research. **Status: experiment required.**

### B10 — Origin misses and network variability

**Resolution:** move predictable work ahead of deadlines and reduce avoidable transfer work; separate managed-LAN service assumptions from WAN forecasts.

**Implementation:** source-aware windows, connection reuse, bounded concurrency, incremental verified delivery where representation permits, and demand-aware scheduling. Maintain an explicit useful-data lead buffer and measure source amplification, retry cost, and verification CPU. Use trusted LAN caches where authorized, with authenticated access and per-tenant isolation. Do not classify a hash-verified peer as authorized merely because its bytes match.

**Acceptance:** test bandwidth/RTT/jitter/outage profiles and bursty accesses, including long averages that look adequate but short windows that are not. Record bytes useful before deadline, not only throughput. A transition barrier waits before exposing an unready scope in the integrated mode; adaptive unmodified play reports actual stalls.

**Failure behavior:** reduce speculative concurrency, preserve active pins, warn of lost readiness, and return explicit failure when required data cannot be obtained. No blanket no-stall WAN guarantee. **Status: implementation and conditional performance qualification.**

### B11 — Read amplification and layout

**Resolution:** independently choose verification, transfer, and allocation geometry, then optimize physical locality.

**Implementation:** first benchmark supported page sizes with coalesced reads/writes. For a new format, evaluate smaller independently authenticated frames, signed range metadata or Bao outboards, and larger physical extents with explicit suballocation only if needed. Publisher compressed chunks retain their real minimum fetch/decode units. Optimize file/slot order without treating logical slot arithmetic as proof of physical contiguity. Do not change an existing arena format in place.

**Acceptance:** measure wire bytes/requested bytes, decoded bytes/requested bytes, disk bytes, metadata size, extents, CPU, and read latency on aligned and boundary-crossing small reads, large sequential reads, and churn. Validate corrupt proofs, truncated input, wrong lengths, and wrong AEAD tags before exposing bytes.

**Failure behavior:** retain a coarser known-correct representation where finer granularity loses overall performance or lacks authenticated proofs. **Status: measured tuning and potentially versioned format work.**

### B12 — Real-game resource pressure and frame times

**Resolution:** make end-to-end performance qualification a first-class product capability, not a conclusion from a storage microbenchmark.

**Implementation:** combine application I/O measurements, ETW/WPR diagnostics, and PresentMon frame data. Test cold and warm starts, low RAM, competing storage load, mapped I/O, queued reads, and the title's actual DirectStorage use if any. Frame timing alone does not identify loading screens or establish storage causation. All comparisons keep security software enabled and use identical game settings and content. [S21]

**Acceptance:** define non-inferiority margins before collecting qualification runs; use paired repeated sessions and uncertainty estimates at the session level. A proposed target is p99/p99.9 frame-time regression within 5% of the native control under the declared matrix, with separately bounded storage-attributable hitches. Report dropped frames, errors, memory, CPU, and startup time; do not discard unfavorable runs without a declared reason.

**Failure behavior:** reduce speculative work or change placement; if the qualified envelope cannot be met, do not advertise native-class play for that configuration. **Status: experiment required.**

### B13 — Preparation cost

**Resolution:** prepare only the admitted closure, while amortizing trust and index construction over immutable content.

**Implementation:** avoid re-encrypting or copying content unnecessarily when an authorized source already provides a suitable trusted representation. Reuse verified unchanged units across builds. Preserve the current batched materializer, and test contiguous-run write coalescing, pipelined decode/verification, and storage-aware ordering within explicit RAM/space reservations. In-place placeholder conversion is an option, not a claim that hashing, trust bootstrap, exclusive access, dehydration, or recovery retention is free.

**Acceptance:** measure total time-to-playable, source bytes, CPU, and peak disk from an existing install and a fresh install. Include import, index/proof creation, materialization, and admission. Compare to the existing 9.27 s materialization result without extrapolating it into an unmeasured whole-game promise.

**Failure behavior:** pre-stage ahead of user launch, show preparation progress, or require a different source/budget. Never dehydrate the only recoverable copy merely to make setup appear fast. **Status: incremental engineering and measurements.**

### B14 — Demand, prefetch, and failure attribution

**Resolution:** keep intent, platform traffic, application completion, and presentation timing as separate evidence streams.

**Implementation:** tag Mirage's own demand/prefetch/maintenance operations explicitly. Preserve callback process information and required/optional ranges without inventing their semantics. Correlate permitted application I/O start/completion with process-instance identity, file identity, request IDs, and timestamps where available. Include an unknown/unmatched class and event-loss counters. Store no command-line secrets in diagnostic exports. [S07], [S21]

**Acceptance:** controlled readers and a synthetic scanner/prefetcher establish ground truth. Measure classifier precision/recall and unmatched events. A successful sealed workload can have zero application failures and many unclassified filesystem misses; report both rather than forcing a zero count by filtering PID 4 or the opener PID.

**Failure behavior:** mark the classification incomplete and retain conservative metrics. No zero-violation certification from incomplete telemetry. **Status: instrumentation and qualification.**

### B15 — Actual disk savings and peak space

**Resolution:** expose two audited quantities: Mirage-managed allocation against its hard budget, and total local game-related footprint including data outside that controller.

**Implementation:** include native files, projected data, arena payloads, metadata, indexes/outboards, journals, temporary downloads, patch references, rollback generations, and retained local origins. Distinguish SSD savings from moving bytes to another local disk or a NAS. Enforce reservations before writes and charge simultaneous copies. Reserve platform hydration closure or decline the corresponding presentation.

**Acceptance:** recorded peaks during install, play, verify, update, rollback, and restore agree with OS allocation measurements within a justified accounting tolerance. The approximately 26.3% synthetic capsule payload is not the whole-product footprint. Verify reclamation after confirming an authorized recoverable source, not merely after an upload call succeeds.

**Failure behavior:** refuse an operation that cannot fit; do not promise that later cleanup will repair a current budget violation. **Status: controller integration and end-to-end qualification.**

### B16 — Recovery and coherence

**Resolution:** reconcile the intersection of authenticated identity, durable metadata, and actual filesystem state.

**Implementation:** use journaled state transitions, immutable generations, valid request epochs, and reader protection. CFAPI's recovery callback is useful but does not attest Mirage's catalog transaction or cryptographic trust. Its update API requires explicit invalidation ranges; do not replace identity while retaining unproven bytes. Keep original/old-generation recovery sources until activation and retention conditions pass. Resolve ambiguous state conservatively instead of blindly making the database match whatever bytes happen to be hydrated. [S06]

**Acceptance:** crash the owned test provider at each transition; test truncated journals, torn payloads, stale replies, delayed readers, slot reuse, disk-full, offline restart, sleep/resume, and interrupted activation. Run the existing long-duration gates in addition to default smoke tests. Verify recoverability and exact bytes, not just successful process restart.

**Failure behavior:** quarantine/re-fetch safely, preserve pins and originals, or require recovery before launch. **Status: protocol design plus failure/endurance qualification.**

### B17 — The engine is not the shipped product

**Resolution:** one supported game-data service and workflow, with the existing writable-drive preview kept explicitly separate.

**Implementation:** reuse `mirage-service`, IPC, and the management UI for discover/classify, choose a feasible budget, prepare, launch through the supported path, diagnose, update, verify, and restore. Projection workers expose capabilities to the coordinator. Privileged setup and per-user work are separated; do not assume all cloud callbacks or services behave identically across sessions. Keep the experiment harness as a regression oracle rather than deleting it when the UI exists.

**Acceptance:** on a clean supported machine, the complete declared game workflow requires no manual devhost, private-pipe commands, or hand-authored profiles. The UI distinguishes prepared-set readiness, verified-scope readiness, adaptive streaming, maintenance, and unsupported cases.

**Failure behavior:** preserve the existing product and data when an optional game backend cannot initialize. **Status: product integration work.**

### B18 — Signing and clean-machine release

**Resolution:** a supported release matrix and authenticated supply chain, not an EV-certificate shortcut.

**Implementation:** sign user-mode artifacts using an appropriate eligible Authenticode route; Microsoft currently documents Artifact Signing, OV/EV certificates, and Store distribution options. Signing does not automatically create SmartScreen reputation or publisher/anti-cheat endorsement. Use package identity where the chosen Windows integration requires it; it is not a universal performance fix. Keep signing keys in protected signing infrastructure and preserve dependency release-age/security policies. Support OS editions/builds still serviced under the declared policy; do not advertise ordinary Windows 10 22H2 as generally supported indefinitely. [S23], [S25]

**Acceptance:** fresh-account installs, standard-user launch, controlled elevation, driver/runtime checks, update, uninstall, recovery, and security-software compatibility pass on the declared matrix. Resolve debug CRT-link hygiene separately; do not suppress warnings to fabricate qualification.

**Failure behavior:** clear unsupported/repair status, no instruction to disable security. **Status: release engineering and external signing eligibility.**

### B19 — Distribution and access rights

**Resolution:** an explicit authorization policy for each source and deployment, reviewed independently of technical capability.

**Implementation:** prefer publisher agreements for integration/CDN use and properly licensed local/LAN deployments. A user owning an installed copy does not grant every redistribution, commercial deployment, protocol use, or peer-sharing right. Sophon's published library code demonstrates a mechanism and has its own MIT license; that license is not a game-content or CDN-access license. Public URLs are not an entitlement model. Scope peer access by authorized tenant/title/version, not only by content hash. [S13], [S28]

**Acceptance:** documented approval/legal review for the intended title, region, access method, hosting, telemetry, and customer deployment. Test expired/revoked access without exposing unauthorized data. Account for whether an old version remains lawfully and technically recoverable.

**Failure behavior:** do not enable that origin/distribution mode. Full-local fallback may remain useful but does not erase rights or storage constraints. **Status: external decision; no technical architecture can certify it alone.**

### B20 — Economics and customer value

**Resolution:** choose the product/deployment from measured total cost and paid operational value, not assumed free bandwidth.

**Implementation:** compare a publisher-integrated offering, an authorized LAN/edge cache, and a qualified consumer mode. Track bytes fetched, amplification, repeated downloads, retention, requests, support incidents, preparation time, and actual SSD saved. Direct publisher or LAN delivery can shift costs away from Mirage hosting; it does not eliminate costs for the customer/provider or eliminate legal dependencies. As one concrete example, R2 documents free direct egress but still charges for storage and operations, with additional service/retrieval conditions. That is a pricing input, not a latency SLA or universal zero-cost model. [S26]

**Acceptance:** obtain paid design-partner evidence and compare margin against storage upgrades and existing alternatives. Include asset/metadata hosting, GET/PUT operations, compute, signing, support, source retention, and incident costs. A chosen partner count is a business milestone, not statistical proof of a market.

**Failure behavior:** change deployment or stop an uneconomic offering before scaling. No revenue guarantee is claimed. **Status: external validation.**

## 4. Experiments that decide the architecture

These are execution specifications. Apart from the read-only platform checks above, they have not been run as part of this research revision.

| Gate | Experiment | Evidence required / failure criterion |
|---|---|---|
| E0: capability inventory | Record OS edition/build, CFAPI platform version, ProjFS state, filesystem/device, driver versions, and BypassIO query results | API presence is recorded separately from behavior. Do not enable features or register roots without the appropriate approval. |
| E1: projection bake-off | On generated disposable copies, compare WinFsp, CFAPI, and ProjFS for buffered, direct, mapped, concurrent, and boundary/EOF reads | Exact bytes, bounded completion, measured callback coverage, allocation growth, and latency. No declaration of native parity from API names. |
| E2: hostile allocation shape | Four large files, a smaller managed budget, tiny scattered reads, long-held handles, mapping, and scanner-like activity | Peak allocation stays in the admitted envelope or admission refuses. Returning under budget after closing files is not a pass. |
| E3: coupled budget/deadline test | Same average goodput, different burst/RTT/outage patterns; known and unknown future scopes | The compiler rejects unsupported hard guarantees; adaptive misses and stalls remain visible. Correct content arrives before each claimed deadline. |
| E4: representation proof | Raw bytes, independent frames, ordinary compressed chunks, and independently encrypted records | No sub-frame transfer claim without a valid representation. Reject tampered proof, wrong length/EOF, truncation, wrong tag, and wrong version. |
| E5: shared-flight and lifetime test | Many same-unit readers, cancellation of most subscribers, delayed completion, process death, concurrent retirement/reuse | One owner reservation; survivors complete; no use-after-reuse, stale publication, deadlock, or cross-tenant disclosure. |
| E6: trace generalization | Hold out players, times, configurations, content versions, and unpredictable branches | Compare to simple policies at equal resources; report deadline misses and confidence rather than only average hit rate. |
| E7: update and repair | New version, old references missing, full scan, rollback, failure at every activation stage | Exact native result or recoverable old state; observed peak fits the operation's reservation. No launcher-state fabrication. |
| E8: game qualification | Owned/non-anti-cheat pilot first, then authorized protected-title tests with supported launch/update paths | Paired frame-time, I/O, memory, compatibility, and full-workflow gates pass for the stated matrix. |
| E9: deployment/business | Clean installations and authorized design-partner operation | Supportable setup/recovery, permission-safe access, actual customer value, and sustainable cost. |

BypassIO requires its own check. Microsoft's documentation says it is per handle, depends on filesystem/filter/storage support, and switches to the traditional path for sparse files. Cached or mapped opens can also suspend it. Merely placing bytes on NTFS, or reverting a placeholder, does not prove every other condition is satisfied. Do not change security/filter policies to make a benchmark pass. [S08]

### Statistical discipline

Use byte identity and state invariants as hard gates. Use measured distributions and uncertainty for performance and prediction claims. Keep failed runs and event-loss information.

For illustration, zero failures in 300 independent representative sessions gives a one-sided 95% binomial upper bound of approximately 0.9936% on per-session failure probability, not proof of zero risk. To put that bound below 0.01% with zero observed failures would require at least 29,956 such independent representative sessions. Real repeated routes are correlated, and software/content changes can invalidate the distributional assumption. These calculations motivate narrow, enforceable contracts rather than marketing certainty.

For performance, begin with paired trials and determine adequate sample counts from native variance. Distinguish a median of per-run p99 values from a pooled percentile. Test warm and true direct paths separately, and include total preparation and peak footprint. Preserve security software and avoid simultaneous builds/tests during timing.

## 5. Repository integration and delivery order

### Reuse the existing architecture

| Existing area | Proposed responsibility |
|---|---|
| `mirage-types`, `mirage-manifest`, `mirage-index`, `mirage-pack` | Version/configuration identities, authenticated range metadata, representation geometry, and readiness-record serialization |
| `mirage-backend` and backend implementations | Capability-advertised authorized origins; exact bounded immutable representation reads |
| `mirage-scheduler` | Shared flights, deadlines, fairness, bounded windows, cancellation, retries, and connection/concurrency control |
| `mirage-engine` | Spatial/temporal admission, the shared get-or-fetch path, scope contracts, generation semantics, and update DAG execution |
| `mirage-cache`, `mirage-db` | Physical accounting, reservations, durable publication, safe leases, reader/writer coordination, and recovery |
| `mirage-predictor`, `mirage-simulator` | Version-bound observations, policy baselines, abstaining prediction, held-out evaluation, and resource-frontier simulation |
| `mirage-etw`, `mirage-observability` | Permitted I/O evidence, event-loss accounting, application outcome correlation, and frame-time integration |
| `mirage-ffi`, `native/winfsp-adapter` | The existing projection and bounded asynchronous bridge; no unsafe driver shortcut |
| `mirage-service`, IPC, management UI | Lifecycle, capability negotiation, readiness states, user budget/consent, maintenance, and supported launch |
| Optional future projection modules | Thin CFAPI or ProjFS adapters only after E0-E2 prove the needed capabilities |

The current `ObjectBackend` includes archive mutation operations. A publisher read adapter should not pretend it can upload/delete publisher content; split capabilities or provide an explicitly read-only adapter boundary. The existing scheduler already has flights and windows; the gap is complete integration and qualification, not absence of all those ideas.

### Dependency order, without speculative calendar estimates

1. Preserve the measured implementation and freeze the terminology for readiness, failures, budgets, and scope.
2. Finish exact profile/build binding and the bounded shared miss path, including owner-only reservations and subscriber cancellation.
3. Establish shared-state lifetime safety before enabling live eviction/promotion across processes.
4. Run E0-E2 on disposable fixtures; choose backends from results, not from the first proposal's tier ordering.
5. Measure a real title's file-size/access-concentration census and collect permitted native lifecycle traces. This determines whether file-granular hydration is economical at all.
6. Benchmark smaller verification units and physical coalescing before committing to a new storage format.
7. Build readiness compilation and compare simple prediction policies before adding population models.
8. Implement update/repair/recovery transactions and the clean service/UI workflow.
9. Qualify allowed real games and pursue the publisher-controlled transition interface with design partners.
10. Enable public modes only when their measured scope, legal access, recovery, and economics gates pass.

The default strategic preference is a tractable authorized LAN/publisher pilot, not a universal public-Genshin promise. The exact best presentation backend remains an experimental decision.

## 6. Designs deliberately rejected or deferred

- **CFAPI as an automatic native-speed, page-budget replacement:** contradicted by unqualified performance and progressive/file-pin behavior.
- **Ignoring required hydration requests or returning zeros to keep the disk small:** violates correctness or application completion.
- **A sparse ordinary NTFS file/VHDX with missing bytes presented as valid data:** holes return zeros; a sparse container alone is not a demand-fetch implementation.
- **A shared bitmap plus a generation check without exclusion:** vulnerable to slot reuse between checking and reading.
- **Assuming Bao bypasses compression, encryption, root trust, or publisher access controls:** false.
- **Repacking proprietary game formats or modifying protected game processes to expose regions:** unnecessary for the byte-preserving design and outside the permitted integration boundary.
- **Classifying every antivirus read as a reason to keep the entire installation native:** can erase the storage benefit and is not a proof of future access.
- **Copying a complete game to another local location while counting only the cache:** hides the real footprint.
- **Replacing current components with many new crates before testing platform assumptions:** creates integration work without resolving the decisive unknowns.

A separate LAN research branch may evaluate block-level presentation using an existing supported iSCSI stack and an authorized target, so NTFS runs above network block storage. Microsoft documents iSCSI application-storage and differencing-disk use cases. That does not prove native SSD latency, publisher acceptance, safe simultaneous mounting of one writable NTFS volume, or a free client cache. Each client needs appropriate exclusive/COW volume semantics, authentication, recovery, and its own performance/rights qualification. This is a B2B alternative to test, not the chosen consumer architecture. [S27]

## 7. Evidence register

Sources below were reviewed or queried during this research. API documentation is evidence for the stated contract, not evidence that Mirage has implemented or qualified the proposed use. Third-party project code is authoritative for that implementation, not for publisher permission.

[S01]: https://learn.microsoft.com/en-us/windows/win32/api/cfapi/nf-cfapi-cfgetplatforminfo
[S02]: https://learn.microsoft.com/en-us/windows/win32/cimwin32prov/win32-optionalfeature
[S03]: https://learn.microsoft.com/en-us/windows/win32/api/cfapi/ne-cfapi-cf_hydration_policy_primary
[S04]: https://learn.microsoft.com/en-us/windows/win32/api/cfapi/ns-cfapi-cf_operation_parameters
[S05]: https://learn.microsoft.com/en-us/windows/win32/api/cfapi/ne-cfapi-cf_pin_state
[S06]: https://learn.microsoft.com/en-us/windows/win32/api/cfapi/nf-cfapi-cfupdateplaceholder
[S07]: https://learn.microsoft.com/en-us/windows/win32/api/cfapi/ns-cfapi-cf_callback_info
[S08]: https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/bypassio
[S09]: https://learn.microsoft.com/en-us/windows/win32/projfs/cache-state
[S10]: https://learn.microsoft.com/en-us/windows/win32/api/projectedfslib/nf-projectedfslib-prjdeletefile
[S11]: https://learn.microsoft.com/en-us/windows/win32/projfs/providing-file-data
[S12]: https://github.com/oconnor663/bao/blob/master/docs/spec.md
[S13]: https://github.com/CollapseLauncher/Hi3Helper.Sophon/tree/9189e990e2d8ef6a9ee5b3dfd77b41e1874f9cac
[S14]: https://github.com/facebook/zstd/blob/dev/contrib/seekable_format/zstd_seekable_compression_format.md
[S15]: https://leboudec.github.io/netcal/
[S16]: https://www.rfc-editor.org/rfc/rfc9110.html#section-14
[S17]: https://www.usenix.org/conference/fast-03/arc-self-tuning-low-overhead-replacement-cache
[S18]: https://arxiv.org/abs/1512.00727
[S19]: https://csrc.nist.gov/pubs/sp/800/226/final
[S20]: https://learn.microsoft.com/en-us/windows/win32/sync/synchronization-and-overlapped-input-and-output
[S21]: https://github.com/GameTechDev/PresentMon/blob/main/README-ConsoleApplication.md
[S22]: https://github.com/winfsp/winfsp/issues/572
[S23]: https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options
[S24]: https://learn.microsoft.com/en-us/windows/msix/package/streaming-install
[S25]: https://learn.microsoft.com/en-us/windows/release-health/supported-versions-windows-client
[S26]: https://developers.cloudflare.com/r2/pricing/
[S27]: https://learn.microsoft.com/en-us/windows-server/storage/iscsi/iscsi-target-server
[S28]: https://genshin.hoyoverse.com/m/en/company/terms
[S29]: https://theupdateframework.github.io/specification/latest/
[S30]: https://learn.microsoft.com/en-us/windows/win32/projfs/projected-file-system

- **S01-S08:** Microsoft Cloud Files and BypassIO contracts, including unsupported `PARTIAL`, progressive growth, asynchronous pin intent, explicit invalidation, process information, and sparse-file/handle limitations.
- **S09-S11:** Microsoft ProjFS cache/view/data behavior. ProjFS is documented for high-speed backing stores; callback range support is not a guarantee of arbitrary range eviction or WAN suitability.
- **S12:** Bao specification, including trusted-root verification, slices, and the final-chunk/EOF rule. Production implementations must follow the actual encoding, not a hand-rolled sketch.
- **S13:** Sophon implementation inspected at commit `9189e990e2d8ef6a9ee5b3dfd77b41e1874f9cac`, dated 2026-09-09. Its manifest schema contains per-file names/sizes/MD5 and chunk names, offsets, compressed/decompressed sizes, and decompressed MD5. Its MIT software license was read. No game-content permission, authenticated fine-grained range proof, or right to CDN access was inferred.
- **S14:** Zstandard seekable format 0.1.0: independent frames and an explicit seek table provide random access; ordinary compressed chunks need not have that structure.
- **S15-S18:** Network-calculus and cache-policy research. The feasibility arithmetic is derived here; algorithm effectiveness remains workload-specific.
- **S19:** NIST SP 800-226, March 2025: formal privacy guarantees need explicit definitions and accounting, not hashed identifiers alone.
- **S20-S21:** Microsoft overlapped-I/O semantics and PresentMon's actual measurement surface. No inference of application scene identity is guaranteed.
- **S22:** A WinFsp issue reporting buffering overhead is a useful hypothesis and matches the direction of local measurements; it is not a complete CPU attribution or a proof that no further improvement exists.
- **S23-S25:** Current Windows signing, streaming-install, and lifecycle guidance. Signing eligibility and supported editions must be checked at release time.
- **S26:** R2 pricing reviewed as an example of nonzero storage/request costs despite free direct egress; no product cost estimate or performance guarantee is inferred.
- **S27:** Microsoft iSCSI storage architecture, supporting a separate authorized LAN investigation rather than a consumer guarantee.
- **S28:** Official publisher terms entry point. Search results expose a personal/noncommercial license description, but the direct page fetch returned no readable body; this research is not a complete legal review and does not establish permission.

- **S29:** TUF specification 1.0.36, dated 2026-08-05, reviewed for update role separation, consistent snapshots, replay/freeze protections, root trust, and explicit non-goals.
- **S30:** Microsoft's ProjFS overview, documenting the intended high-speed-backing-store use case and the absence of cloud-recall progress/offline-state features.

## Final conclusion

The defensible unifying idea is **capability-selected presentation plus compiled data readiness, with separate spatial, temporal, and trust admission**. Prediction is an optimizer; authenticated content is the byte authority; the coordinator is the lifetime authority; measurements and publisher permission determine which modes can be offered.

All twenty bottlenecks have an explicit resolution path and a falsifiable acceptance gate. Several require experiments and external decisions, and the large uncached-read performance gap remains unresolved in the implementation. No CFAPI/ProjFS migration, new game-content distribution, or real-game compatibility certification was performed by writing this proposal.

## Implementation status (2026-09-14)

| Step (Section 5) | Status |
|---|---|
| 1. Terminology freeze | Done — readiness-v1 freezes the five modes, scope completeness, the spatial and temporal inequalities, and the fetch-failure taxonomy. |
| 2. Exact profile/build binding and the bounded shared miss path | Done — B08 binding rejects wrong repository, manifest, schema, or configuration labels; the miss path now shares one flight per page with owner-only reservation, subscriber detachment, a bounded fetch pool, typed failure causes, and ledger commit after placement. |
| 3. Shared-state lifetime safety | Done as the safe restriction — published slots of a sealed mounted generation are excluded from reclaim; the full cross-process reader-lease protocol is not implemented. |
| 4. E0-E2 experiments | Partially — the E0 read-only capability inventory is implemented and was run on the research machine; E1/E2 have not been run. |
| 5. Real-title census and native lifecycle traces | Not started. |
| 6. Page-geometry benchmarks | Partially — the E4 representation-proof gates are covered by tests; 64 KiB, 256 KiB, and 1 MiB were measured on a synthetic fixture (`docs/experiments/page-geometry-2026-09-14.md`). B09's adjacent-slot read coalescing (`mirage_cache::read_contiguous`, used by the engine and the WinFsp host) and B13's contiguous-run write coalescing in `insert_reserved_pages` are implemented and measured: with both, a sequential 1 MiB read costs 0.81-0.87 ms at every page size on the unbuffered arena (3.90 ms at 64 KiB before), and arena extents at 64 KiB fell from 3,729 to 265. The read-only origin boundary (`ReadOnlyOrigin`, `ObjectBackend::capabilities`) from this section is implemented. Verification-unit (B11) benchmarks and real-title measurements were not run; the ~65% penalty on large uncached reads through WinFsp was not re-measured and is not claimed closed. |
| 7. Readiness compilation and simple prediction policies | Partially — the readiness compiler and equal-budget baselines with abstention are implemented; population models are not started. |
| 8. Update/repair/recovery transactions and service/UI workflow | Not started beyond existing code. |
| 9-10. Real-game qualification and publisher interface | External; not actionable in this repository. |

The WinFsp host still serves misses from a local origin directly (`crates/mirage-ffi/src/lib.rs`, `read_span_from_origin`) and is not yet bridged to `PageProvider::get_or_fetch`; that bridge needs a DB-writer boundary decision (ADR 0006) before it can be built.
