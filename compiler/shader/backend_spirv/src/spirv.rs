//! The SPIR-V numbers the backend uses (SPIR-V 1.6 and the extensions it relies on), named as
//! the grammar names them; every number was checked against Khronos' `spirv.core.grammar.json` and
//! `extinst.glsl.std.450.grammar.json` (add new ones the same way).

macro_rules! table {
    ($(#[$meta:meta])* $mod:ident { $($name:ident = $value:literal),* $(,)? }) => {
        $(#[$meta])*
        #[allow(non_upper_case_globals, dead_code)]
        pub mod $mod {
            $(pub const $name: u32 = $value;)*

            /// Every entry, by its grammar name.
            pub const ALL: &[(&str, u32)] = &[$((stringify!($name), $value)),*];

            pub fn name(value: u32) -> Option<&'static str> {
                ALL.iter().find(|(_, v)| *v == value).map(|(n, _)| *n)
            }
        }
    };
}

table! {
    /// Instructions (without their `Op` prefix).
    op {
        Nop = 0, Undef = 1, Name = 5, MemberName = 6, String = 7, Extension = 10,
        ExtInstImport = 11, ExtInst = 12, MemoryModel = 14, EntryPoint = 15, ExecutionMode = 16,
        Capability = 17, TypeVoid = 19, TypeBool = 20, TypeInt = 21, TypeFloat = 22,
        TypeVector = 23, TypeMatrix = 24, TypeImage = 25, TypeSampler = 26, TypeSampledImage = 27, TypeArray = 28,
        TypeRuntimeArray = 29, TypeStruct = 30, TypePointer = 32, TypeFunction = 33,
        ConstantTrue = 41, ConstantFalse = 42, Constant = 43, ConstantComposite = 44,
        ConstantNull = 46, SpecConstantOp = 52, Function = 54, FunctionParameter = 55,
        FunctionEnd = 56, FunctionCall = 57, Variable = 59, Load = 61, Store = 62,
        AccessChain = 65, InBoundsAccessChain = 66, ArrayLength = 68, Decorate = 71, MemberDecorate = 72,
        VectorExtractDynamic = 77, VectorInsertDynamic = 78, VectorShuffle = 79,
        CompositeConstruct = 80, CompositeExtract = 81, CompositeInsert = 82, CopyObject = 83,
        Transpose = 84, SampledImage = 86, ImageSampleImplicitLod = 87,
        ImageSampleExplicitLod = 88, ImageSampleDrefImplicitLod = 89,
        ImageSampleDrefExplicitLod = 90, ImageFetch = 95, ImageGather = 96, ImageDrefGather = 97,
        ImageRead = 98, ImageWrite = 99, ImageQuerySizeLod = 103, ImageQuerySize = 104,
        ImageQueryLevels = 106, ImageQuerySamples = 107,
        ConvertFToU = 109, ConvertFToS = 110, ConvertSToF = 111,
        ConvertUToF = 112, UConvert = 113, SConvert = 114, FConvert = 115, Bitcast = 124,
        SNegate = 126, FNegate = 127, IAdd = 128, FAdd = 129, ISub = 130, FSub = 131,
        IMul = 132, FMul = 133, UDiv = 134, SDiv = 135, FDiv = 136, UMod = 137, SRem = 138,
        FRem = 140, VectorTimesMatrix = 144, MatrixTimesVector = 145,
        MatrixTimesMatrix = 146, Dot = 148, UMulExtended = 151, SMulExtended = 152,
        IsNan = 156, IsInf = 157, LogicalEqual = 164, LogicalNotEqual = 165, LogicalOr = 166,
        LogicalAnd = 167, LogicalNot = 168, Select = 169, IEqual = 170, INotEqual = 171,
        UGreaterThan = 172, SGreaterThan = 173, UGreaterThanEqual = 174,
        SGreaterThanEqual = 175, ULessThan = 176, SLessThan = 177, ULessThanEqual = 178,
        SLessThanEqual = 179, FOrdEqual = 180, FUnordEqual = 181, FOrdNotEqual = 182,
        FUnordNotEqual = 183, FOrdLessThan = 184, FUnordLessThan = 185,
        FOrdGreaterThan = 186, FUnordGreaterThan = 187, FOrdLessThanEqual = 188,
        FUnordLessThanEqual = 189, FOrdGreaterThanEqual = 190, FUnordGreaterThanEqual = 191,
        ShiftRightLogical = 194, ShiftRightArithmetic = 195, ShiftLeftLogical = 196,
        BitwiseOr = 197, BitwiseXor = 198, BitwiseAnd = 199, Not = 200, BitFieldInsert = 201,
        BitFieldSExtract = 202, BitFieldUExtract = 203, BitReverse = 204, BitCount = 205,
        ControlBarrier = 224, MemoryBarrier = 225, AtomicLoad = 227, AtomicStore = 228,
        AtomicExchange = 229, AtomicCompareExchange = 230, AtomicIAdd = 234, AtomicISub = 235,
        AtomicSMin = 236, AtomicUMin = 237, AtomicSMax = 238, AtomicUMax = 239,
        AtomicAnd = 240, AtomicOr = 241, AtomicXor = 242, Phi = 245, LoopMerge = 246,
        SelectionMerge = 247, Label = 248, Branch = 249, BranchConditional = 250, Switch = 251,
        Return = 253, ReturnValue = 254, Unreachable = 255, ExecutionModeId = 331,
        DecorateId = 332, GroupNonUniformElect = 333, GroupNonUniformAll = 334,
        GroupNonUniformAny = 335, GroupNonUniformAllEqual = 336,
        GroupNonUniformBroadcast = 337, GroupNonUniformBroadcastFirst = 338,
        GroupNonUniformBallot = 339, GroupNonUniformShuffle = 345,
        GroupNonUniformIAdd = 349, GroupNonUniformFAdd = 350, GroupNonUniformIMul = 351,
        GroupNonUniformFMul = 352, GroupNonUniformSMin = 353, GroupNonUniformUMin = 354,
        GroupNonUniformFMin = 355, GroupNonUniformSMax = 356, GroupNonUniformUMax = 357,
        GroupNonUniformFMax = 358, GroupNonUniformBitwiseAnd = 359,
        GroupNonUniformBitwiseOr = 360, GroupNonUniformBitwiseXor = 361,
        GroupNonUniformLogicalAnd = 362, GroupNonUniformLogicalOr = 363,
        GroupNonUniformLogicalXor = 364, GroupNonUniformQuadBroadcast = 365,
        GroupNonUniformQuadSwap = 366, CopyLogical = 400,
        TypeUntypedPointerKHR = 4417, UntypedVariableKHR = 4418,
        UntypedAccessChainKHR = 4419, UntypedInBoundsAccessChainKHR = 4420,
        UntypedArrayLengthKHR = 4425, TypeBufferEXT = 5115,
        TypeRayQueryKHR = 4472, RayQueryInitializeKHR = 4473, RayQueryConfirmIntersectionKHR = 4476,
        RayQueryProceedKHR = 4477, RayQueryGetIntersectionTypeKHR = 4479,
        BufferPointerEXT = 5119, ConstantSizeOfEXT = 5129, EmitMeshTasksEXT = 5294,
        SetMeshOutputsEXT = 5295, TypeAccelerationStructureKHR = 5341,
        RayQueryGetIntersectionTKHR = 6018, RayQueryGetIntersectionInstanceCustomIndexKHR = 6019,
        RayQueryGetIntersectionInstanceIdKHR = 6020, RayQueryGetIntersectionGeometryIndexKHR = 6022,
        RayQueryGetIntersectionPrimitiveIndexKHR = 6023,
        RayQueryGetIntersectionBarycentricsKHR = 6024, RayQueryGetIntersectionFrontFaceKHR = 6025,
    }
}

table! {
    capability {
        Matrix = 0, Shader = 1, Float16 = 9, Float64 = 10, Int64 = 11, Int64Atomics = 12,
        Int16 = 22, SampleRateShading = 35, SampledCubeArray = 45, ImageMSArray = 48,
        ImageQuery = 50,
        StorageImageReadWithoutFormat = 55, StorageImageWriteWithoutFormat = 56, GroupNonUniform = 61, GroupNonUniformVote = 62,
        GroupNonUniformArithmetic = 63, GroupNonUniformBallot = 64,
        GroupNonUniformShuffle = 65, GroupNonUniformClustered = 67, GroupNonUniformQuad = 68,
        DrawParameters = 4427, RayQueryKHR = 4472, ShaderNonUniform = 5301, StorageBuffer16BitAccess = 4433, StoragePushConstant16 = 4435,
        UntypedPointersKHR = 4473, DescriptorHeapEXT = 5128, MeshShadingEXT = 5283,
        RuntimeDescriptorArray = 5302,
    }
}

table! {
    decoration {
        Block = 2, ColMajor = 5, ArrayStride = 6, MatrixStride = 7, BuiltIn = 11,
        NoPerspective = 13, Flat = 14, Centroid = 16, Sample = 17, NonWritable = 24,
        NonReadable = 25, Location = 30, Index = 32, Binding = 33, DescriptorSet = 34,
        Offset = 35,
        ArrayStrideIdEXT = 5124, PerPrimitiveEXT = 5271, NonUniform = 5300,
    }
}

table! {
    builtin {
        Position = 0, ClipDistance = 3, PrimitiveId = 7, FragCoord = 15, FrontFacing = 17,
        SampleId = 18, SampleMask = 20, FragDepth = 22, WorkgroupId = 26,
        LocalInvocationId = 27, GlobalInvocationId = 28, LocalInvocationIndex = 29,
        SubgroupSize = 36, SubgroupLocalInvocationId = 41, VertexIndex = 42, InstanceIndex = 43,
        BaseInstance = 4425, PrimitivePointIndicesEXT = 5294, PrimitiveLineIndicesEXT = 5295,
        PrimitiveTriangleIndicesEXT = 5296, CullPrimitiveEXT = 5299,
        SamplerHeapEXT = 5122, ResourceHeapEXT = 5123,
    }
}

table! {
    storage_class {
        UniformConstant = 0, Input = 1, Uniform = 2, Output = 3, Workgroup = 4, Private = 6,
        Function = 7, PushConstant = 9, StorageBuffer = 12, TaskPayloadWorkgroupEXT = 5402,
    }
}

table! {
    scope { Device = 1, Workgroup = 2, Subgroup = 3 }
}

table! {
    /// Memory semantics bits.
    semantics {
        AcquireRelease = 0x8, UniformMemory = 0x40, WorkgroupMemory = 0x100,
    }
}

table! {
    execution_model { Vertex = 0, Fragment = 4, GLCompute = 5, TaskEXT = 5364, MeshEXT = 5365 }
}

table! {
    execution_mode {
        OriginUpperLeft = 7, DepthReplacing = 12, LocalSize = 17, OutputVertices = 26,
        OutputPoints = 27, OutputLinesEXT = 5269, OutputPrimitivesEXT = 5270,
        OutputTrianglesEXT = 5298,
    }
}

table! {
    group_operation { Reduce = 0, InclusiveScan = 1, ExclusiveScan = 2, ClusteredReduce = 3 }
}

table! {
    /// `GLSL.std.450` extended instructions.
    glsl {
        RoundEven = 2, Trunc = 3, FAbs = 4, SAbs = 5, FSign = 6, SSign = 7, Floor = 8, Ceil = 9,
        Fract = 10, Sin = 13, Cos = 14, Tan = 15, Asin = 16, Acos = 17, Atan = 18, Sinh = 19,
        Cosh = 20, Tanh = 21, Atan2 = 25, Pow = 26, Exp = 27, Log = 28, Exp2 = 29, Log2 = 30,
        Sqrt = 31, InverseSqrt = 32, FMin = 37, UMin = 38, SMin = 39, FMax = 40, UMax = 41,
        SMax = 42, Step = 48, SmoothStep = 49, Fma = 50, Length = 66, Distance = 67,
        Cross = 68, Normalize = 69, FaceForward = 70, Reflect = 71, Refract = 72,
        FindILsb = 73, FindSMsb = 74, FindUMsb = 75,
    }
}

/// The SPIR-V magic number and the version this backend writes (1.6).
pub const MAGIC: u32 = 0x0723_0203;
pub const VERSION_1_6: u32 = 0x0001_0600;
