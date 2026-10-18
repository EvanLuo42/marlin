//! The pass manager and the optimization passes (`docs/design/0005-shader-compiler.md` §4, step
//! 8 of §11).
//!
//! Every pass keeps the IR's structured control flow (0006 §5.1): regions stay single-entry,
//! single-exit, and properly nested, so the verifier accepts the module after every pass. In
//! Development compilers the [`PassManager`] checks that after each pass that changed
//! something (0005 §4.6).
//!
//! Step 8a:
//! - [`PassManager`]: passes over a module, or over one function at a time
//!   ([`FunctionPass`]), with the control-flow analyses of each function cached until a pass
//!   changes its control flow ([`Analyses`]).
//! - [`SimplifyCfg`] (0005 §4.4 pass 2.12): constant branches folded, unreachable blocks
//!   removed (an unreachable merge or continue block stays, as `unreachable`), switches and
//!   loops that no longer branch or loop turned into jumps when nothing breaks out of them,
//!   straight-line blocks merged, empty blocks jumped over.
//! - [`Dce`] (2.10 without control dependence): instructions without side effects, and block
//!   parameters, that nothing live reads.
//! - [`Sccp`] (2.1): sparse conditional constant propagation; values and branches it proves
//!   constant become constants, for [`SimplifyCfg`] to fold.
//!
//! Step 8b, stages 0 and 1:
//! - [`DivGuard`] (0.2): integer division and remainder by 0 give 0 (0004 §9.6).
//! - [`StripDebug`] (0.4): `Debug.*` goes in Shipping builds; Development builds keep it for
//!   each backend.
//! - [`Inline`] (1.1): every call, with `return`s turned into breaks out of a loop that runs
//!   once (through a flag from inside the callee's loops and switches).
//! - [`Sroa`] (1.2) and [`Mem2Reg`] (1.3): variables split into parts and made SSA values.
//! - [`PrecisePropagate`] (1.6), [`HoistConstArrays`] (1.7), [`GlobalDce`] (1.8).
//!
//! Passes 1.4 and 1.5 come with the DXIL backend, the only one that needs them (0005 §11).
//!
//! Step 8c: [`uniformity`], which values and blocks may differ between the lanes of a wave or
//! a group, and [`CheckBarriers`], which rejects (E1401, a [`Problem`]) a group barrier some
//! threads may miss, after inlining.

use std::collections::HashMap;
use std::sync::Arc;

use marlin_shaderc_ir::cfg::{Cfg, Structure};
use marlin_shaderc_ir::{FuncId, Function, Module, VerifyError, verify_module};

mod check_barriers;
mod dce;
mod edit;
mod global_dce;
mod guard;
mod hoist;
mod inline;
mod mem2reg;
mod precise;
mod sccp;
mod simplify_cfg;
mod sroa;
mod strip_debug;
pub mod uniformity;

pub use check_barriers::CheckBarriers;
pub use dce::Dce;
pub use global_dce::GlobalDce;
pub use guard::DivGuard;
pub use hoist::HoistConstArrays;
pub use inline::Inline;
pub use mem2reg::Mem2Reg;
pub use precise::PrecisePropagate;
pub use sccp::Sccp;
pub use simplify_cfg::SimplifyCfg;
pub use sroa::{MAX_ELEMENTS, Sroa};
pub use strip_debug::StripDebug;

/// The two builds (0005 §5.4): both fully optimized; they differ in `Debug.*` and debug
/// information.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Profile {
    Development,
    Shipping,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub profile: Profile,
    /// Verify the module after every pass that changed it (Development compilers), not only
    /// at the end.
    pub verify_each: bool,
}

/// What a pass did to a function.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Changed {
    No,
    /// Instructions and values, not the blocks or jumps: control-flow analyses still hold.
    Values,
    /// The control flow.
    Cfg,
}

/// A function's control-flow analyses.
#[derive(Clone, Debug)]
pub struct FlowInfo {
    pub cfg: Cfg,
    pub structure: Structure,
}

impl FlowInfo {
    pub fn new(f: &Function) -> FlowInfo {
        let cfg = Cfg::new(f);
        let structure = Structure::new(f, &cfg);
        FlowInfo { cfg, structure }
    }
}

/// Analysis results, per function, kept until a pass changes what they describe.
#[derive(Default)]
pub struct Analyses {
    flow: HashMap<FuncId, Arc<FlowInfo>>,
}

impl Analyses {
    pub fn flow(&mut self, id: FuncId, f: &Function) -> Arc<FlowInfo> {
        self.flow
            .entry(id)
            .or_insert_with(|| Arc::new(FlowInfo::new(f)))
            .clone()
    }

    fn invalidate(&mut self, id: FuncId, changed: Changed) {
        if changed == Changed::Cfg {
            self.flow.remove(&id);
        }
    }
}

/// A pass over a whole module.
pub trait Pass {
    fn name(&self) -> &'static str;
    /// Whether it changed anything.
    fn run(&self, m: &mut Module, analyses: &mut Analyses) -> bool;
    /// What is wrong with the program that the pass found (a check pass): the pipeline stops
    /// with them.
    fn problems(&self, _m: &Module) -> Vec<Problem> {
        Vec::new()
    }
}

/// Something wrong with the program that only the optimizer sees (after inlining, say).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub code: &'static str,
    pub message: String,
    /// Where: an index into the module's source locations.
    pub at: Option<u32>,
    /// Why, and where that is.
    pub because: Option<(String, Option<u32>)>,
}

/// A pass over one function at a time. While it runs, the function is taken out of the module:
/// the pass may add types and constants but must not look at other functions.
pub trait FunctionPass {
    fn name(&self) -> &'static str;
    fn run_function(
        &self,
        m: &mut Module,
        id: FuncId,
        f: &mut Function,
        analyses: &mut Analyses,
    ) -> Changed;
}

impl<P: FunctionPass> Pass for P {
    fn name(&self) -> &'static str {
        FunctionPass::name(self)
    }

    fn run(&self, m: &mut Module, analyses: &mut Analyses) -> bool {
        let ids: Vec<FuncId> = m.funcs.iter().map(|(id, _)| id).collect();
        let mut any = false;
        for id in ids {
            let mut f = std::mem::replace(&mut m.funcs[id], Function::new("", Vec::new()));
            let changed = self.run_function(m, id, &mut f, analyses);
            m.funcs[id] = f;
            analyses.invalidate(id, changed);
            any |= changed != Changed::No;
        }
        any
    }
}

#[derive(Clone, Debug)]
pub enum PassError {
    /// The module did not verify after a pass: a bug in the pass.
    Verify {
        pass: &'static str,
        errors: Vec<VerifyError>,
    },
    /// A check found the program wrong.
    Rejected {
        pass: &'static str,
        problems: Vec<Problem>,
    },
}

impl std::fmt::Display for PassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PassError::Verify { pass, errors } => {
                write!(f, "the module does not verify after `{pass}`")?;
                for e in errors {
                    write!(f, "\n  {e}")?;
                }
            }
            PassError::Rejected { pass, problems } => {
                write!(f, "`{pass}` rejected the program")?;
                for p in problems {
                    write!(f, "\n  error[{}]: {}", p.code, p.message)?;
                }
            }
        }
        Ok(())
    }
}

/// Which passes ran, and whether each changed the module.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub ran: Vec<(&'static str, bool)>,
}

pub struct PassManager {
    passes: Vec<Box<dyn Pass>>,
    verify_each: bool,
}

impl PassManager {
    pub fn new(verify_each: bool) -> PassManager {
        PassManager {
            passes: Vec::new(),
            verify_each,
        }
    }

    pub fn add(&mut self, pass: impl Pass + 'static) -> &mut Self {
        self.passes.push(Box::new(pass));
        self
    }

    pub fn add_boxed(&mut self, pass: Box<dyn Pass>) -> &mut Self {
        self.passes.push(pass);
        self
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.passes.iter().map(|p| p.name()).collect()
    }

    /// Runs the passes in order; the module verifies at the end (and after each pass that
    /// changed it, with `verify_each`).
    pub fn run(&self, m: &mut Module) -> Result<Report, PassError> {
        let mut analyses = Analyses::default();
        let mut report = Report::default();
        for pass in &self.passes {
            let changed = pass.run(m, &mut analyses);
            report.ran.push((pass.name(), changed));
            let problems = pass.problems(m);
            if !problems.is_empty() {
                return Err(PassError::Rejected {
                    pass: pass.name(),
                    problems,
                });
            }
            if changed && self.verify_each {
                check(m, pass.name())?;
            }
        }
        if !self.verify_each {
            check(m, "the pipeline")?;
        }
        Ok(report)
    }
}

fn check(m: &Module, pass: &'static str) -> Result<(), PassError> {
    let errors = verify_module(m);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(PassError::Verify { pass, errors })
    }
}

/// The pipeline for `options` (0005 §4.4; the passes there so far).
pub fn pipeline(options: &Options) -> PassManager {
    let mut pm = PassManager::new(options.verify_each);
    // Stage 0.
    pm.add(DivGuard);
    if options.profile == Profile::Shipping {
        pm.add(StripDebug);
    }
    // Stage 1.
    pm.add(Inline)
        .add(SimplifyCfg)
        .add(Sroa)
        .add(Mem2Reg)
        .add(PrecisePropagate)
        .add(HoistConstArrays)
        .add(GlobalDce);
    // Stage 2, so far; barriers are checked once constants have folded what they can.
    pm.add(SimplifyCfg)
        .add(Sccp)
        .add(SimplifyCfg)
        .add(CheckBarriers)
        .add(Dce)
        .add(SimplifyCfg);
    pm
}
