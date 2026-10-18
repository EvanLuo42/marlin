//! The closed set of operations (0006 §8, §9). Immediates (field indices, comparison kinds,
//! scopes, branch targets) live in the [`Op`]; every value an instruction reads is an operand of
//! the instruction, in the order the text format writes them, so use lists see all of them.
//!
//! Texture, ray query, mesh and packing operations join the enum together with the stages and
//! backends that need them (0005 §11 step 10).

use crate::func::{Block, FuncId};
use crate::ty::Space;

words! {
    /// Two-operand arithmetic; integer division and remainder by zero give 0 (0006 §10).
    BinaryOp {
        Add = "add", Sub = "sub", Mul = "mul", SDiv = "sdiv", UDiv = "udiv", SRem = "srem",
        URem = "urem", And = "and", Or = "or", Xor = "xor", Shl = "shl", LShr = "lshr",
        AShr = "ashr", MulHiS = "mulhi.s", MulHiU = "mulhi.u", FAdd = "fadd", FSub = "fsub",
        FMul = "fmul", FDiv = "fdiv", FRem = "frem",
    }
}

impl BinaryOp {
    pub fn is_float(self) -> bool {
        matches!(
            self,
            BinaryOp::FAdd | BinaryOp::FSub | BinaryOp::FMul | BinaryOp::FDiv | BinaryOp::FRem
        )
    }
}

words! {
    UnaryOp { Neg = "neg", Not = "not", FNeg = "fneg" }
}

words! {
    IntCmp {
        Eq = "eq", Ne = "ne", Slt = "slt", Sle = "sle", Sgt = "sgt", Sge = "sge", Ult = "ult",
        Ule = "ule", Ugt = "ugt", Uge = "uge",
    }
}

words! {
    /// Ordered (`o*`: false if either side is NaN) and unordered (`u*`: true if either is).
    FloatCmp {
        Oeq = "oeq", One = "one", Olt = "olt", Ole = "ole", Ogt = "ogt", Oge = "oge",
        Ord = "ord", Uno = "uno", Ueq = "ueq", Une = "une", Ult = "ult", Ule = "ule",
        Ugt = "ugt", Uge = "uge",
    }
}

words! {
    /// Float to integer saturates, NaN gives 0 (0006 §10).
    ConvertOp {
        SExt = "sext", ZExt = "zext", Trunc = "trunc", FpExt = "fpext", FpTrunc = "fptrunc",
        SiToFp = "sitofp", UiToFp = "uitofp", FpToSi = "fptosi", FpToUi = "fptoui",
        Bitcast = "bitcast",
    }
}

words! {
    /// The core set and the replaceable set (0006 §8.2).
    MathOp {
        Fma = "fma", Sqrt = "sqrt", Rsqrt = "rsqrt", Rcp = "rcp", Exp2 = "exp2", Log2 = "log2",
        Sin = "sin", Cos = "cos", Abs = "abs", SMin = "smin", SMax = "smax", UMin = "umin",
        UMax = "umax", FMin = "fmin", FMax = "fmax", Floor = "floor", Ceil = "ceil",
        FTrunc = "ftrunc", RoundEven = "round_even", Fract = "fract", Sign = "sign", Dot = "dot",
        Cross = "cross", Popcount = "popcount", FirstBitHiS = "firstbit_hi.s",
        FirstBitHiU = "firstbit_hi.u", FirstBitLo = "firstbit_lo", BitReverse = "bitreverse",
        BitfieldExtractS = "bitfield_extract.s", BitfieldExtractU = "bitfield_extract.u",
        BitfieldInsert = "bitfield_insert", IsNan = "isnan", IsInf = "isinf",
        Pow = "pow", Tan = "tan", Asin = "asin", Acos = "acos", Atan = "atan", Atan2 = "atan2",
        Sinh = "sinh", Cosh = "cosh", Tanh = "tanh", Exp = "exp", Log = "log",
        Normalize = "normalize", Length = "length", Distance = "distance", Reflect = "reflect",
        Refract = "refract", Smoothstep = "smoothstep", Step = "step",
        FaceForward = "faceforward",
    }
}

impl MathOp {
    /// How many operands it takes.
    pub fn arity(self) -> usize {
        use MathOp::*;
        match self {
            Fma | BitfieldExtractS | BitfieldExtractU | Refract | Smoothstep | FaceForward => 3,
            BitfieldInsert => 4,
            SMin | SMax | UMin | UMax | FMin | FMax | Dot | Cross | Pow | Atan2 | Distance
            | Reflect | Step => 2,
            _ => 1,
        }
    }

    /// Whether a target may lack it and get the standard library's implementation instead.
    pub fn is_replaceable(self) -> bool {
        use MathOp::*;
        matches!(
            self,
            Pow | Tan
                | Asin
                | Acos
                | Atan
                | Atan2
                | Sinh
                | Cosh
                | Tanh
                | Exp
                | Log
                | Normalize
                | Length
                | Distance
                | Reflect
                | Refract
                | Smoothstep
                | Step
                | FaceForward
        )
    }
}

words! {
    AtomicOp {
        Add = "add", Sub = "sub", And = "and", Or = "or", Xor = "xor", SMin = "smin",
        SMax = "smax", UMin = "umin", UMax = "umax", Xchg = "xchg", CmpXchg = "cmpxchg",
        Load = "load", Store = "store",
    }
}

impl AtomicOp {
    /// How many values it takes after its pointer (or texel): none to load, two to compare and
    /// exchange.
    pub fn values(self) -> usize {
        match self {
            AtomicOp::Load => 0,
            AtomicOp::CmpXchg => 2,
            _ => 1,
        }
    }
}

words! {
    Scope { Lane = "lane", Wave = "wave", Group = "group", Device = "device" }
}

words! {
    MemOrder { Relaxed = "relaxed", Acquire = "acquire", Release = "release", AcqRel = "acq_rel" }
}

words! {
    Axis { X = "ddx", Y = "ddy" }
}

words! {
    ReduceOp {
        Add = "add", FAdd = "fadd", Mul = "mul", FMul = "fmul", SMin = "smin", SMax = "smax",
        UMin = "umin", UMax = "umax", FMin = "fmin", FMax = "fmax", And = "and", Or = "or",
        Xor = "xor",
    }
}

words! {
    /// `read_lane` takes the lane in the quad as an operand.
    QuadOp { SwapX = "swap_x", SwapY = "swap_y", SwapDiag = "swap_diag", ReadLane = "read_lane" }
}

words! {
    VoteOp { Any = "any", All = "all", Equal = "equal" }
}

words! {
    /// What `rq.get` reads of an intersection (§8.9).
    RqField {
        Kind = "kind", T = "t", Barycentrics = "barycentrics", InstanceIndex = "instance_index",
        InstanceId = "instance_id", PrimitiveIndex = "primitive_index",
        GeometryIndex = "geometry_index", FrontFace = "front_face",
    }
}

words! {
    /// The operands of a texture operation after its coordinate (§8.6), always in this order;
    /// `grad` is two operands (the derivatives along x and y). `offset` is a constant integer
    /// vector; `compare` the depth reference of a comparison sampler.
    TexArg {
        Lod = "lod", Bias = "bias", Grad = "grad", Layer = "layer", Mip = "mip",
        Sample = "sample", Offset = "offset", Compare = "compare",
    }
}

words! {
    /// What `tex.query` asks of a texture (§8.6): the size of mip 0 (or of a `mip` operand),
    /// the layers (cubes, for cube arrays), the mips, the samples.
    TexQuery {
        Size = "size", Layers = "layers", Mips = "mips", Samples = "samples",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WaveOp {
    Reduce(ReduceOp),
    Scan { op: ReduceOp, exclusive: bool },
    Ballot,
    Broadcast,
    Shuffle,
    Quad(QuadOp),
    Vote(VoteOp),
    Match,
    LaneIndex,
    LaneCount,
    IsFirst,
}

/// One step of an `access`: a struct field by index, or an array, vector or matrix element
/// whose index is the next operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AccessStep {
    Field(u32),
    Index,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SwitchCase {
    /// The case value's bits, at the scrutinee's width.
    pub value: u64,
    pub block: Block,
    /// How many operands are this target's arguments.
    pub args: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Op {
    // ---- terminators (0006 §5.1) ----
    /// Operands: the target's arguments.
    Br(Block),
    /// Operands: the condition, the true target's arguments, the false target's arguments.
    CondBr {
        then_block: Block,
        then_args: u32,
        else_block: Block,
        merge: Block,
    },
    /// Operands: the scrutinee, then each case's arguments, then the default's.
    Switch {
        cases: Vec<SwitchCase>,
        default: Block,
        default_args: u32,
        merge: Block,
    },
    /// Enters a loop; the block it ends is the loop's preheader. Operands: the header's
    /// arguments.
    Loop {
        header: Block,
        continuing: Block,
        merge: Block,
    },
    /// Operands: the returned values.
    Return,
    Unreachable,

    // ---- arithmetic, comparison, conversion (§8.1) ----
    Binary(BinaryOp),
    Unary(UnaryOp),
    Icmp(IntCmp),
    Fcmp(FloatCmp),
    /// `select %c, %a, %b`; a vector condition selects per component.
    Select,
    /// The result type says what to convert to.
    Convert(ConvertOp),

    // ---- math (§8.2) ----
    Math(MathOp),

    // ---- vectors, matrices, aggregates (§8.3) ----
    Splat,
    Extract(u32),
    Insert(u32),
    Shuffle(Vec<u32>),
    ExtractDyn,
    InsertDyn,
    MatMul,
    Transpose,
    MatColumn(u32),
    MatRow(u32),
    MatExtract(u32, u32),
    /// Builds the result type from its parts.
    Construct,
    ExtractField(u32),
    InsertField(u32),
    ExtractElem(u32),
    InsertElem(u32),

    // ---- memory (§6) and resources (§7) ----
    /// A `function`-space variable of the result's pointee type.
    Var,
    Load {
        aligned: Option<u32>,
    },
    /// Operands: the pointer, the value.
    Store {
        aligned: Option<u32>,
    },
    /// Operands: the base pointer, then one index per `Index` step.
    Access(Vec<AccessStep>),
    /// Operands: a `Buffer`/`ReadOnlyBuffer` handle, the element index.
    BufferElem,
    /// Operands: a `ByteBuffer`/`ReadOnlyByteBuffer` handle, the byte offset (4-aligned).
    BufferByte,
    UniformPtr,
    BufferLength,
    DescGet {
        nonuniform: bool,
    },

    // ---- atomics and barriers (§8.4) ----
    /// Operands: the pointer, then the value(s): none for `load`, two for `cmpxchg`.
    Atomic {
        op: AtomicOp,
        scope: Scope,
        order: MemOrder,
    },
    /// `exec` is `None` for a memory-only barrier.
    Barrier {
        exec: Option<Scope>,
        mem: Scope,
        spaces: Vec<Space>,
    },

    // ---- pixel (§5.2, §8.5) ----
    Discard,
    IsHelper,
    Deriv {
        axis: Axis,
        fine: bool,
    },

    // ---- wave (§8.7) ----
    Wave {
        op: WaveOp,
        cluster: Option<u32>,
    },

    // ---- textures (§8.6) ----
    /// Operands: the texture, the sampler, the coordinate, then one per argument. Without
    /// `lod`, `bias` or `grad` the level comes from derivatives (pixel entries only).
    TexSample(Vec<TexArg>),
    /// Operands: the texture (sampled or storage), the integer coordinate, then one per
    /// argument (`layer`, `mip`, `sample`).
    TexLoad(Vec<TexArg>),
    /// Operands: the storage texture, the integer coordinate, the value, then `layer`.
    TexStore(Vec<TexArg>),
    /// Operands: the storage texture, the integer coordinate, the values (as for `atomic`), then
    /// `layer`. Result: the texel before, an integer scalar of 32 or 64 bits (§8.4).
    TexAtomic {
        op: AtomicOp,
        scope: Scope,
        order: MemOrder,
        args: Vec<TexArg>,
    },
    /// Operands: the texture, the sampler, the coordinate, then `layer`, `offset` and
    /// `compare`. Result: one channel (0 to 3, red to alpha) of the four texels a bilinear
    /// sample would use; with `compare`, their comparison results (channel 0).
    TexGather {
        channel: u8,
        args: Vec<TexArg>,
    },
    /// Operands: the texture, then (for `size` of a sampled texture) the mip. Result: `u32`,
    /// or `u32x2`/`u32x3` for the size.
    TexQuery(TexQuery),

    // ---- ray queries (§8.9) ----
    /// Operands: the query (a pointer to a `rayquery` variable), the acceleration structure,
    /// the ray's origin, t-min, direction and t-max, the flags, the instance mask.
    RqInit,
    /// Operand: the query. Result: whether there is a candidate to look at.
    RqProceed,
    /// Operand: the query. A field of the committed intersection, or the candidate's.
    RqGet {
        committed: bool,
        field: RqField,
    },
    /// Operand: the query. Commits the candidate triangle.
    RqCommitTriangle,

    // ---- mesh and amplification (§8.9) ----
    /// Operands: the vertex count, the primitive count; before any mesh output is written.
    MeshSetCounts,
    /// Operands: the mesh groups to launch, x, y, z. The payload is the module's `payload`
    /// variable; the amplification entry returns right after.
    MeshDispatch,

    // ---- uniformity (0004 §10.3) ----
    /// The operand, asserted to be the same in every lane of the group (`[Uniform]`).
    AssumeUniform,
    /// The operand, to be treated as different in each lane (`NonUniform(x)`).
    AssumeNonUniform,

    // ---- calls and debugging (§8.8, §8.10) ----
    Call(FuncId),
    /// Operands: the values the format refers to.
    DebugPrint(u32),
    /// Operands: the condition, then the values.
    DebugAssert(u32),
}

impl Op {
    pub fn is_terminator(&self) -> bool {
        matches!(
            self,
            Op::Br(_)
                | Op::CondBr { .. }
                | Op::Switch { .. }
                | Op::Loop { .. }
                | Op::Return
                | Op::Unreachable
        )
    }

    /// The blocks a terminator jumps to, with the range of operands that are each one's
    /// arguments. The merge and continue blocks of `cond_br`, `switch` and `loop` are not
    /// jumps.
    pub fn successors(&self, operands: usize) -> Vec<(Block, std::ops::Range<usize>)> {
        match self {
            Op::Br(b) => vec![(*b, 0..operands)],
            Op::CondBr {
                then_block,
                then_args,
                else_block,
                ..
            } => {
                let t = *then_args as usize;
                vec![(*then_block, 1..1 + t), (*else_block, 1 + t..operands)]
            }
            Op::Switch {
                cases,
                default,
                default_args,
                ..
            } => {
                let mut out = Vec::new();
                let mut at = 1;
                for case in cases {
                    out.push((case.block, at..at + case.args as usize));
                    at += case.args as usize;
                }
                out.push((*default, at..at + *default_args as usize));
                out
            }
            Op::Loop { header, .. } => vec![(*header, 0..operands)],
            _ => Vec::new(),
        }
    }

    /// The blocks a terminator names without jumping there: merge and continue blocks.
    pub fn structure(&self) -> Vec<Block> {
        match self {
            Op::CondBr { merge, .. } | Op::Switch { merge, .. } => vec![*merge],
            Op::Loop {
                continuing, merge, ..
            } => vec![*continuing, *merge],
            _ => Vec::new(),
        }
    }
}
