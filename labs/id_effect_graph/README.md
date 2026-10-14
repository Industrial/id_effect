# id_effect_graph

Directed acyclic graphs and topological sorting for [`id_effect`](https://github.com/Industrial/id_effect) programs.

- **[`Dag`]** — explicit edge list with Kahn topological sort
- **[`DependencyNode`]** + **[`topological_sort`]** — capability-style `requires` / `provides` resolution (extracted from the capability planner)
- **[`GraphError`]** — duplicate ids, missing dependencies, cycles

See the mdBook chapter *Events and projections* (`part5/ch23-00-events-and-projections.md`).
