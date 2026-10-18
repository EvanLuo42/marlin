use marlin_shader::reflect::{
    Attribute, ConstValue, EntryInfo, Extension, FieldLayout, FieldValue, NamedValue,
    ShaderReflection, SourceHash, Stage, StageBinding, StageVariable, Stats, StructLayout,
    register,
};

fn sample() -> ShaderReflection {
    ShaderReflection {
        entry: EntryInfo {
            name: "Game.Resolve<Game.Rock>".into(),
            stage: Stage::Compute,
            group_size: [64, 1, 1],
        },
        inputs: vec![StageVariable {
            name: "id".into(),
            ty: "i32x3".into(),
            binding: StageBinding::SystemValue("DispatchThreadId".into()),
        }],
        outputs: vec![StageVariable {
            name: "Color1".into(),
            ty: "f32x4".into(),
            binding: StageBinding::Target {
                index: 0,
                source: 1,
            },
        }],
        push: Some(StructLayout {
            name: "Game.Args".into(),
            size: 16,
            fields: vec![
                FieldLayout {
                    name: "Out".into(),
                    ty: "Buffer<uint>".into(),
                    offset: 0,
                    size: 4,
                    attributes: vec![],
                },
                FieldLayout {
                    name: "Scale".into(),
                    ty: "float3".into(),
                    offset: 4,
                    size: 12,
                    attributes: vec![Attribute {
                        name: "Range".into(),
                        args: vec!["0".into(), "2".into()],
                    }],
                },
            ],
        }),
        static_params: vec![NamedValue {
            name: "Game.Rock.UseMoss".into(),
            ty: "bool".into(),
            value: ConstValue::Bool(true),
        }],
        exports: vec![NamedValue {
            name: "Topology".into(),
            ty: "Game.ClosureInfo".into(),
            value: ConstValue::Struct(vec![
                FieldValue {
                    name: "Count".into(),
                    value: ConstValue::UInt(2),
                },
                FieldValue {
                    name: "Weights".into(),
                    value: ConstValue::List(vec![ConstValue::Float(0.25), ConstValue::Int(-1)]),
                },
            ]),
        }],
        capabilities: vec!["Int64".into()],
        target_constants: vec![],
        sources: vec![SourceHash {
            package: "Game".into(),
            path: "Resolve.mshader".into(),
            hash: 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210,
        }],
        stats: Stats {
            instructions: 42,
            groupshared_bytes: 256,
            code_bytes: 1024,
        },
        extensions: vec![Extension {
            name: "spirv".into(),
            data: vec![1, 2, 3],
        }],
    }
}

#[test]
fn reflection_round_trips_through_the_serial_format() {
    let r = sample();
    let bytes = marlin_serial::to_bytes(&r).unwrap();
    let back: ShaderReflection = marlin_serial::from_bytes(&bytes).unwrap();
    assert_eq!(back, r);
    let empty = ShaderReflection::default();
    let back: ShaderReflection =
        marlin_serial::from_bytes(&marlin_serial::to_bytes(&empty).unwrap()).unwrap();
    assert_eq!(back, empty);
}

#[test]
fn reflection_types_register() {
    register().unwrap();
    let info = marlin_reflect::type_info::<ShaderReflection>();
    assert!(marlin_reflect::find(info.id).is_some());
    // Registering again is not a collision.
    register().unwrap();
}
