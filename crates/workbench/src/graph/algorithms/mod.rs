//! The Graph page's server-side algorithm implementations (GRA-01..04), each scoped to a
//! [`super::csr_view::ScopedAdjacency`] built by [`super::csr_view::build_scoped_adjacency`] —
//! never re-filtering edge types independently (D-01/D-04).

pub mod betweenness;
pub mod community;
pub mod k_core;
pub mod paths;
pub mod ward_subgraph;
