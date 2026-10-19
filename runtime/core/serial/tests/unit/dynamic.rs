//! Dynamic holders: values of types chosen at run time, written with their type's id.

use marlin_reflect::{
    DynamicHolder, Ref, Reflect, ReflectDynamic, TypeInfo, TypeKind, TypeName, Value, find_data,
};
use marlin_serial::{from_bytes, to_bytes};

/// Type data: types a `Holder` accepts.
struct Allowed;

#[derive(Default)]
struct Holder(Option<Value>);

impl DynamicHolder for Holder {
    fn held(&self) -> Option<Ref<'_>> {
        self.0.as_ref().map(Value::as_ref)
    }

    fn hold(&mut self, value: Value) -> Result<(), Value> {
        if Self::accepts(value.info()) {
            self.0 = Some(value);
            Ok(())
        } else {
            Err(value)
        }
    }

    fn accepts(info: &'static TypeInfo) -> bool {
        find_data::<Allowed>(info).is_some()
    }
}

// SAFETY: an opaque description of `Holder` itself.
unsafe impl Reflect for Holder {
    const TYPE_NAME: TypeName = TypeName::simple("serial_test::Holder");
    const TYPE_INFO: TypeInfo = TypeInfo {
        data: &[&ReflectDynamic::of::<Self>()],
        ..TypeInfo::new::<Self>(TypeKind::Opaque)
    };

    fn reflect_default() -> Option<Self> {
        Some(Self(None))
    }

    fn reflect_clone(&self) -> Option<Self> {
        Some(Self(match &self.0 {
            Some(v) => Some(v.try_clone()?),
            None => None,
        }))
    }
}

#[derive(Reflect, Debug, Clone, PartialEq, Default)]
#[reflect(name = "serial_test::Light")]
struct Light {
    intensity: f32,
    color: [f32; 3],
}

#[derive(Reflect, Debug, Clone, PartialEq, Default)]
#[reflect(name = "serial_test::Tag")]
struct Tag;

#[derive(Reflect, Debug, Clone, PartialEq, Default)]
#[reflect(name = "serial_test::Secret")]
struct Secret(u32);

#[derive(Reflect, Default)]
#[reflect(name = "serial_test::Bag", default)]
struct Bag {
    items: Vec<Holder>,
    after: u32,
}

fn allow<T: Reflect>() {
    marlin_reflect::register::<T>().unwrap();
    marlin_reflect::add_data(marlin_reflect::type_info::<T>(), Allowed).unwrap();
}

#[test]
fn held_values_round_trip_with_their_types() {
    allow::<Light>();
    allow::<Tag>();
    marlin_reflect::register::<Secret>().unwrap();
    let light = Light {
        intensity: 800.0,
        color: [1.0, 0.5, 0.25],
    };
    let bag = Bag {
        items: vec![
            Holder(Some(Value::new(light.clone()))),
            Holder(Some(Value::new(Tag))),
            Holder(None),
            // Registered but not accepted: written, read back as empty.
            Holder(Some(Value::new(Secret(7)))),
        ],
        after: 42,
    };
    let bytes = to_bytes(&bag).unwrap();
    let back: Bag = from_bytes(&bytes).unwrap();
    assert_eq!(back.items.len(), 4);
    let held = |i: usize| back.items[i].held();
    assert_eq!(held(0).and_then(|r| r.downcast::<Light>()), Some(&light));
    assert_eq!(held(1).and_then(|r| r.downcast::<Tag>()), Some(&Tag));
    assert!(held(2).is_none());
    assert!(
        held(3).is_none(),
        "a type the holder does not accept is not constructed"
    );
    assert_eq!(back.after, 42, "what follows reads normally");
}
