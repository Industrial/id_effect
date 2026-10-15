# Layers — Building Your Dependency Graph

You've seen how `R` encodes what an effect needs and how an environment value (an HList) holds
the capabilities. But who *builds* the environment when capabilities have to be loaded, connected
or opened?

In small programs and tests you build it by hand with `hlist![..]` and `provide_env`. In real
applications you describe each construction as a **`Layer<Out, E, In>`**: a lazy recipe that builds
the capabilities `Out` from the capabilities `In`, failing with `E` (Effect.ts `Layer`).
`effect.provide_layer(layer)` builds the graph, runs the effect against its output and then
releases the layers' resources.

Layers are typed like effects, so the graph is checked by the compiler: a capability nobody
produces is a compile error at `provide_layer`, and there is no runtime planner and no build order
to get wrong.

This chapter covers what a layer is, the layer constructors (and the provider derive that writes
the simple ones), composing layers with `merge` / `provide_to` / `provide_merge`, and how whole
application graphs are wired, memoised and torn down.
