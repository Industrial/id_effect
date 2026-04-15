//! `layer_node!` and `layer_graph!` macros for compact layer-planner DSL.

/// Build a single [`LayerNode`](crate::LayerNode).
///
/// ```ignore
/// let node = id_effect::layer_node!(
///   "repo",
///   requires = ["Db", "Cache"],
///   provides = ["Repo"]
/// );
/// ```
#[macro_export]
macro_rules! layer_node {
  ($id:expr, requires = [$($req:expr),* $(,)?], provides = [$($prov:expr),* $(,)?]) => {
    ::id_effect::LayerNode::new($id, [$($req),*], [$($prov),*])
  };
}

/// Build a [`LayerGraph`](crate::LayerGraph) from a compact declaration block.
///
/// ```ignore
/// let graph = id_effect::layer_graph! {
///   db    => [Db];
///   cache => [Cache];
///   repo  : [Db, Cache] => [Repo];
///   api   : [Repo] => [Api];
/// };
/// ```
#[macro_export]
macro_rules! layer_graph {
  (
    $(
      $id:ident $( : [$($req:ident),* $(,)?] )? => [$($prov:ident),* $(,)?]
    );+ $(;)?
  ) => {
    ::id_effect::LayerGraph::new([
      $(
        ::id_effect::LayerNode::new(
          stringify!($id),
          $crate::layer_graph!(@reqs $( [$($req),*] )?),
          $crate::layer_graph!(@provs [$( $prov ),*]),
        )
      ),+
    ])
  };

  (@reqs) => {
    ::std::vec::Vec::<&'static str>::new()
  };

  (@reqs [$($req:ident),*]) => {
    ::std::vec![$( stringify!($req) ),*]
  };

  (@provs [$($prov:ident),*]) => {
    ::std::vec![$( stringify!($prov) ),*]
  };
}
