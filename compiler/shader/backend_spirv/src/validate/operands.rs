//! What each instruction's operands are, so the validator can tell ids from literals: a
//! string of kinds, one per operand, in order. Checked against Khronos'
//! `spirv.core.grammar.json` like the numbers in `spirv.rs`.
//!
//! - `t` the result type, `r` the result id, `i` an id, `l` a literal word (numbers and
//!   enumerants), `s` a literal string;
//! - a kind followed by `*` repeats to the end, by `?` may be left out (the rest are optional
//!   too);
//! - `L` the literal words of a constant (as wide as the type), `C` a switch's (literal,
//!   label) pairs, `M` memory operands (a mask, then a literal for `Aligned`).
//!
//! Where the grammar writes an enumerant that brings its own operands (a decoration's, an
//! execution mode's, loop controls), the layout spells them out: literals (`l*`), or ids for
//! the `Id` forms (`i*`); `OpSpecConstantOp`'s operands are the ids of its operation.

use crate::spirv::op;

pub fn layout(opcode: u32) -> Option<&'static str> {
    Some(match opcode {
        op::Nop => "",
        op::Undef => "tr",
        op::Name => "is",
        op::MemberName => "ils",
        op::String => "rs",
        op::Extension => "s",
        op::ExtInstImport => "rs",
        op::ExtInst => "trili*",
        op::MemoryModel => "ll",
        op::EntryPoint => "lisi*",
        op::ExecutionMode => "ill*",
        op::Capability => "l",
        op::TypeVoid | op::TypeBool | op::TypeSampler => "r",
        op::TypeInt => "rll",
        op::TypeFloat => "rll?",
        op::TypeVector | op::TypeMatrix => "ril",
        op::TypeImage => "rilllllll?",
        op::TypeSampledImage => "ri",
        op::SampledImage => "trii",
        // Image operands: a mask, then its ids.
        op::ImageSampleImplicitLod
        | op::ImageSampleExplicitLod
        | op::ImageFetch
        | op::ImageRead => "triil?i*",
        op::ImageWrite => "iiil?i*",
        op::TypeArray => "rii",
        op::TypeRuntimeArray => "ri",
        op::TypeStruct => "ri*",
        op::TypePointer => "rli",
        op::TypeFunction => "rii*",
        op::ConstantTrue | op::ConstantFalse | op::ConstantNull => "tr",
        op::Constant => "trL",
        op::ConstantComposite => "tri*",
        op::SpecConstantOp => "trli*",
        op::Function => "trli",
        op::FunctionParameter => "tr",
        op::FunctionEnd => "",
        op::FunctionCall => "trii*",
        op::Variable => "trli?",
        op::Load => "triM",
        op::Store => "iiM",
        op::AccessChain | op::InBoundsAccessChain => "trii*",
        op::ArrayLength => "tril",
        op::Decorate => "ill*",
        op::MemberDecorate => "illl*",
        op::DecorateId => "ili*",
        op::VectorExtractDynamic => "trii",
        op::VectorInsertDynamic => "triii",
        op::VectorShuffle => "triil*",
        op::CompositeConstruct => "tri*",
        op::CompositeExtract => "tril*",
        op::CompositeInsert => "triil*",
        op::CopyObject | op::CopyLogical | op::Transpose => "tri",
        op::ConvertFToU
        | op::ConvertFToS
        | op::ConvertSToF
        | op::ConvertUToF
        | op::UConvert
        | op::SConvert
        | op::FConvert
        | op::Bitcast
        | op::SNegate
        | op::FNegate
        | op::Not
        | op::LogicalNot
        | op::IsNan
        | op::IsInf
        | op::BitReverse
        | op::BitCount => "tri",
        op::IAdd
        | op::FAdd
        | op::ISub
        | op::FSub
        | op::IMul
        | op::FMul
        | op::UDiv
        | op::SDiv
        | op::FDiv
        | op::UMod
        | op::SRem
        | op::FRem
        | op::VectorTimesMatrix
        | op::MatrixTimesVector
        | op::MatrixTimesMatrix
        | op::Dot
        | op::UMulExtended
        | op::SMulExtended
        | op::LogicalEqual
        | op::LogicalNotEqual
        | op::LogicalOr
        | op::LogicalAnd
        | op::IEqual
        | op::INotEqual
        | op::UGreaterThan
        | op::SGreaterThan
        | op::UGreaterThanEqual
        | op::SGreaterThanEqual
        | op::ULessThan
        | op::SLessThan
        | op::ULessThanEqual
        | op::SLessThanEqual
        | op::FOrdEqual
        | op::FUnordEqual
        | op::FOrdNotEqual
        | op::FUnordNotEqual
        | op::FOrdLessThan
        | op::FUnordLessThan
        | op::FOrdGreaterThan
        | op::FUnordGreaterThan
        | op::FOrdLessThanEqual
        | op::FUnordLessThanEqual
        | op::FOrdGreaterThanEqual
        | op::FUnordGreaterThanEqual
        | op::ShiftRightLogical
        | op::ShiftRightArithmetic
        | op::ShiftLeftLogical
        | op::BitwiseOr
        | op::BitwiseXor
        | op::BitwiseAnd => "trii",
        op::Select | op::BitFieldSExtract | op::BitFieldUExtract => "triii",
        op::BitFieldInsert => "triiii",
        op::ControlBarrier => "iii",
        op::MemoryBarrier => "ii",
        op::AtomicLoad => "triii",
        op::AtomicStore => "iiii",
        op::AtomicExchange
        | op::AtomicIAdd
        | op::AtomicISub
        | op::AtomicSMin
        | op::AtomicUMin
        | op::AtomicSMax
        | op::AtomicUMax
        | op::AtomicAnd
        | op::AtomicOr
        | op::AtomicXor => "triiii",
        op::AtomicCompareExchange => "triiiiii",
        op::Phi => "tri*",
        op::LoopMerge => "iil*",
        op::SelectionMerge => "il",
        op::Label => "r",
        op::Branch => "i",
        op::BranchConditional => "iiil*",
        op::Switch => "iiC*",
        op::Return | op::Unreachable => "",
        op::ReturnValue => "i",
        op::ExecutionModeId => "ili*",
        op::GroupNonUniformElect => "tri",
        op::GroupNonUniformAll | op::GroupNonUniformAny | op::GroupNonUniformAllEqual => "trii",
        op::GroupNonUniformBroadcast | op::GroupNonUniformShuffle => "triii",
        op::GroupNonUniformBroadcastFirst | op::GroupNonUniformBallot => "trii",
        op::GroupNonUniformIAdd
        | op::GroupNonUniformFAdd
        | op::GroupNonUniformIMul
        | op::GroupNonUniformFMul
        | op::GroupNonUniformSMin
        | op::GroupNonUniformUMin
        | op::GroupNonUniformFMin
        | op::GroupNonUniformSMax
        | op::GroupNonUniformUMax
        | op::GroupNonUniformFMax
        | op::GroupNonUniformBitwiseAnd
        | op::GroupNonUniformBitwiseOr
        | op::GroupNonUniformBitwiseXor
        | op::GroupNonUniformLogicalAnd
        | op::GroupNonUniformLogicalOr
        | op::GroupNonUniformLogicalXor => "trilii?",
        op::GroupNonUniformQuadBroadcast | op::GroupNonUniformQuadSwap => "triii",
        op::TypeUntypedPointerKHR => "rl",
        op::UntypedVariableKHR => "trli?i?",
        op::UntypedAccessChainKHR | op::UntypedInBoundsAccessChainKHR => "triii*",
        op::UntypedArrayLengthKHR => "triil",
        op::TypeBufferEXT => "rl",
        op::BufferPointerEXT => "tri",
        op::ConstantSizeOfEXT => "tri",
        op::SetMeshOutputsEXT => "ii",
        op::TypeRayQueryKHR | op::TypeAccelerationStructureKHR => "r",
        op::RayQueryInitializeKHR => "iiiiiiii",
        op::RayQueryConfirmIntersectionKHR => "i",
        op::RayQueryProceedKHR => "tri",
        op::RayQueryGetIntersectionTypeKHR
        | op::RayQueryGetIntersectionTKHR
        | op::RayQueryGetIntersectionInstanceCustomIndexKHR
        | op::RayQueryGetIntersectionInstanceIdKHR
        | op::RayQueryGetIntersectionGeometryIndexKHR
        | op::RayQueryGetIntersectionPrimitiveIndexKHR
        | op::RayQueryGetIntersectionBarycentricsKHR
        | op::RayQueryGetIntersectionFrontFaceKHR => "trii",
        op::EmitMeshTasksEXT => "iiii?",
        _ => return None,
    })
}
