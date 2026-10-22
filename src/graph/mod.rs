// Relationships:
// - `src/analysis/dependencies.rs`: Passes the detected dependencies to the stages
// - `src/diagram.rs`: Returns the generated diagram to the caller
// - `src/error.rs`: Reports why a diagram could not be generated
// - `src/graph/components.rs`: Groups the scanned files into components
// - `src/graph/layout.rs`: Lays the diagram out around the module
// - `src/graph/reconcile.rs`: Finds relationships in the blocks and the code
// - `src/options.rs`: Passes the generation settings to the stages
// - `src/repo.rs`: Passes the scanned repository to the grouping stage

//! Turns the scanned repository into a diagram: groups files into
//! components, reconciles each Relationships block with the dependencies
//! detected in the code, and lays the result out around the chosen module.

mod block;
mod components;
mod layout;
mod reconcile;

use crate::analysis::Analysis;
use crate::diagram::Diagram;
use crate::error::Error;
use crate::options::Options;
use crate::repo::Repo;

use components::World;

/// Builds the diagram of `module`, a path relative to the repository root.
pub(crate) fn build(
    repo: &Repo,
    analysis: &Analysis,
    module: &str,
    module_is_file: bool,
    options: &Options,
) -> Result<Diagram, Error> {
    let world = World::new(repo, analysis);
    let scope = layout::scope(&world, module, module_is_file)?;
    let (edges, findings) = reconcile::reconcile(&world, analysis, options);
    layout::assemble(&world, &scope, module, edges, findings, options)
}
