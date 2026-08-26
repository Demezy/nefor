# Nefor MAG in Five Minutes

## 1. A complete agent graph

```lisp
(require "nefor.actors")
(require "nefor.artifact")
(require "nefor.contracts")
(require "nefor.graph")

(let start (nefor.actors.task-source "task" "Inspect the repository."))
(let worker (nefor.actors.agent
        (as nefor.actors.AgentConfig
          {:id "worker"
           :model (nefor.contracts.no-identifier)
           :profile (nefor.contracts.identifier "standard")
           :provider "chatgpt"
           :system "Answer the task."
           ;; The shared profile includes "mag-eval", but not lead orchestration.
           ;; Tool calls carry a concrete 1–5-word intent describing why they run.
           :tools nefor.actors.general-tools
           :da-policy (nefor.contracts.no-da-policy)
           :max-corrections 2})
        (type-tag nefor.contracts.Task)
        "task"
        (type-tag nefor.contracts.TextAnswer)))
(let result (nefor.graph.output "result"
        (type-tag (| nefor.contracts.TextAnswer nefor.contracts.AgentError))))
;; A graph is an immutable edge set. Edge endpoints carry their complete node
  ;; definitions, so there is no separate add-node operation.
  (nefor.artifact.compile
    (fn [[graph nefor.graph.Graph]] -> nefor.graph.Graph
      (nefor.graph.add-edges graph
        [(nefor.graph.edge start worker)
         (nefor.graph.edge worker result)])))
```

When calling `mag-eval`, supply a 1–5-word `intent` describing the world-level
operation the expression performs.

## 2. Graph algebra and typed boundaries

```lisp
;; These functions return new Graph values. Duplicate additions collapse and
;; removing an absent edge returns the same graph.
(nefor.graph.graph edges)
(nefor.graph.add-edges graph added-edges)
(nefor.graph.remove-edges graph removed-edges)

;; Reusing one bound node in several edges is fan-out.
[(nefor.graph.edge producer left-consumer)
 (nefor.graph.edge producer right-consumer)]

;; Sending several producers into one product input is fan-in. (+ A B) fires
;; after A and B arrive. A union (| A B) fires for either arriving constructor.
;; A Unit edge carries ordering without domain data. Cycles are ordinary edges.
```

A `Node I O` stores low-level actors, routes, messages, and one public input and
output. Connecting nodes checks `O` against the next `I`. The graph contains
values; constructing it starts no actors and performs no work.

## 3. Functions construct larger nodes

```lisp
;; Node-producing functions are ordinary MAG functions. The caller sees only
;; Task -> (TextAnswer | AgentError); lowering later expands the internals.
(let make-worker
  (fn [[config nefor.actors.AgentConfig]]
    -> (nefor.graph.Node nefor.contracts.Task
         (| nefor.contracts.TextAnswer nefor.contracts.AgentError))
    (nefor.actors.agent
      config
      (type-tag nefor.contracts.Task)
      "task"
      (type-tag nefor.contracts.TextAnswer))))

(let worker-1 (make-worker first-config))
(let worker-2 (make-worker second-config))
(let review-cycle (make-review-cycle worker-1 reviewer build))
;; review-cycle is itself one Node whose internals may contain both workers,
  ;; the reviewer, the deterministic build, and every feedback route. It is
  ;; connected exactly like a primitive node.
  [(nefor.graph.edge task review-cycle)
   (nefor.graph.edge review-cycle result)]
```

The same mechanism can package an agent as one node, that node inside a review
cycle, and several review cycles inside a release workflow. Hierarchy is the
authoring form; the final artifact contains the flattened low-level actors and
routes.

## 4. Compile-time mapping and runtime-sized fan-out

```lisp
;; This map runs while MAG constructs the artifact. `known-tasks` must already
;; be an immutable List Task in the source snapshot.
(map make-worker known-tasks)

;; Runtime tasks are different. A planner emits List Task only after execution
;; has started. A resident rule names a pure MAG function that receives that
;; list and returns a graph-delta artifact.
(type Task {:task String :description String :dependent_tasks (List String)})
(type WorkerResult {:task String :description String})
(type IndexedTask {:index Int :value Task})

(let indexed-task
  (fn [[index Int] [task Task]] -> IndexedTask
    (as IndexedTask {:index index :value task})))

(let worker-id
  (fn [[entry IndexedTask]] -> String
    (str "expand.worker." (get entry "index"))))

(let expand
  (fn [[tasks (List Task)]] -> Artifact
    (if (= (count tasks) 0)
      ;; The zero-worker case still sends an empty collected value onward.
      (nefor.artifact.delta
        (nefor.graph.delta
          (as (List nefor.graph.Actor) [])
          (as (List nefor.graph.StoredRoute) [])
          [(nefor.dynamic.empty-to summary-input)]
          (as (List String) [])))
      ((fn [] -> Artifact
      (let indexed (indexed-map indexed-task tasks))
      (let sender-ids (map
              (fn [[entry IndexedTask]] -> String
                (str (worker-id entry) ".llm"))
              indexed))
      (let collector (nefor.dynamic.collector
              "expand.collector"
              sender-ids
              (type-tag (| WorkerResult nefor.contracts.AgentError))))
      (let with-workers (fold
              (fn [[change nefor.graph.Delta] [entry IndexedTask]]
                -> nefor.graph.Delta
                (add-worker (get collector "input") change entry))
              (nefor.graph.node-delta collector)
              indexed))
      (let completed (nefor.graph.delta-route
              with-workers
              (get collector "output")
              summary-input))
      (nefor.artifact.delta completed))))))

;; planner-success has type Port (List Task). When it fires, the runtime invokes
;; the named `expand` function in the retained MAG environment and atomically
;; interprets and applies the returned raw Delta artifact.
(nefor.graph.rule "expand" planner-success "expand")
```

The dynamic mechanism is still library composition: planner, rule, workers,
collector, and integrator are graph data. A reusable `MapNodes` constructor
could package this shape without becoming a MAG language primitive.

## 5. Processes and worktrees are nodes

```lisp
;; Structured argv inserts no shell. The command runs only when this node fires.
(nefor.process.exec "build"
  (as nefor.process.ProcessExecParams
    {:argv ["cargo" "build"]
     :cwd worktree-path
     :timeout (nefor.contracts.no-timeout)}))

;; Shell syntax is explicit POSIX /bin/sh -c work.
(nefor.shell.script "pipeline"
  (as nefor.shell.ShellScriptParams
    {:script "rg -n TODO src/ | sort"
     :cwd worktree-path
     :timeout (nefor.contracts.timeout-ms 30000)}))

;; A worktree constructor returns Node Unit Worktree. Bind the node once; its
;; typed output carries the generated path to agents and deterministic commands.
(let worktree
  (nefor.worktree.create "task-worktree"
    (as nefor.worktree.CreateSpec
      {:repository repository
       :path requested-path
       :branch branch
       :base base})))
```

`ProcessResult` keeps stdout, stderr, and termination status. A nonzero exit is
data rather than an evaluator failure, so task-specific code can classify it:

```lisp
(type BuildPassed {:stdout String})
(type BuildFailed {:stdout String :stderr String :status Int})
(type BuildFeedback (| BuildPassed BuildFailed))

(let classify-build
  (fn [[result nefor.contracts.ProcessResult]] -> BuildFeedback
    (let termination (get result "termination"))
    (if (= (get termination "kind") "exit")
        (if (= (get termination "value") 0)
          (as BuildFeedback
            (as BuildPassed {:stdout (get result "stdout")}))
          (as BuildFeedback
            (as BuildFailed
              {:stdout (get result "stdout")
               :stderr (get result "stderr")
               :status (get termination "value")})))
        (as BuildFeedback
          (as BuildFailed
            {:stdout (get result "stdout")
             :stderr (get result "stderr")
             :status (get termination "value")})))))
```

The build command, policy that prevents the builder from running it itself,
classification function, and feedback input all belong beside the workflow
that uses them. Nefor supplies the typed mechanisms; it does not prescribe one
SDLC.
