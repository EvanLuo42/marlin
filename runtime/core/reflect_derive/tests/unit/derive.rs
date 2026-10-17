//! `#[derive(Reflect)]` features beyond the basics covered by marlin-reflect's own tests:
//! editor metadata, doc capture, lifetimes, const generics, trait casts and type data. The
//! derive's compile errors are covered by the `compile_fail` examples in its crate docs.

use std::borrow::Cow;

use marlin_reflect::{
    ConstArg, Mut, Ref, Reflect, ReflectTrait, TypeId, TypeKind, TypeName, Widget, type_info,
};

/// A point light.
///
/// Lights the scene around it.
#[derive(Reflect, Debug, Default, PartialEq)]
#[reflect(
    display_name = "Point Light",
    category = "Lighting",
    meta(icon = "bulb", order = "2"),
    version = 3,
    impl_trait(Describe)
)]
struct Light {
    /// Brightness in EV.
    #[reflect(range = 0.0..=10.0, step = 0.25, units = "EV", widget = Slider)]
    intensity: f32,
    #[reflect(widget = Color, display_name = "Tint", category = "Look")]
    color: [f32; 3],
    #[reflect(widget = Angle, units = "deg", doc = "Cone angle.")]
    /// Ignored: the explicit `doc` wins.
    angle: f32,
    #[reflect(widget = "knob", meta(min_label = "off", max_label = "loud"))]
    hum: f32,
    plain: u8,
}

trait Describe {
    fn describe(&self) -> String;
    fn bump(&mut self);
}

impl Describe for Light {
    fn describe(&self) -> String {
        format!("light at {} EV", self.intensity)
    }

    fn bump(&mut self) {
        self.intensity += 1.0;
    }
}

#[test]
fn field_metadata() {
    let info = type_info::<Light>();
    let intensity = info.field("intensity").unwrap();
    let a = &intensity.attrs;
    assert_eq!(a.doc, Some("Brightness in EV."));
    assert_eq!(a.step, Some(0.25));
    assert_eq!(a.units, Some("EV"));
    assert_eq!(a.widget, Some(Widget::Slider));
    assert_eq!(a.range.unwrap().max, 10.0);
    assert_eq!(intensity.label(), "intensity");

    let color = info.field("color").unwrap();
    assert_eq!(color.attrs.widget, Some(Widget::Color));
    assert_eq!(color.attrs.display_name, Some("Tint"));
    assert_eq!(color.label(), "Tint");
    assert_eq!(color.attrs.category, Some("Look"));
    assert_eq!(color.attrs.doc, None);

    let angle = info.field("angle").unwrap();
    assert_eq!(angle.attrs.doc, Some("Cone angle."));
    assert_eq!(angle.attrs.widget, Some(Widget::Angle));

    let hum = info.field("hum").unwrap();
    assert_eq!(hum.attrs.widget, Some(Widget::Custom("knob")));
    assert_eq!(
        hum.attrs.meta,
        &[("min_label", "off"), ("max_label", "loud")][..]
    );
    assert_eq!(hum.attrs.meta_value("max_label"), Some("loud"));
    assert_eq!(hum.attrs.meta_value("nope"), None);

    let plain = info.field("plain").unwrap();
    assert!(plain.attrs.widget.is_none() && plain.attrs.meta.is_empty());
}

#[test]
fn type_metadata() {
    let attrs = &type_info::<Light>().attrs;
    assert_eq!(
        attrs.doc,
        Some("A point light.\n\nLights the scene around it.")
    );
    assert_eq!(attrs.display_name, Some("Point Light"));
    assert_eq!(attrs.category, Some("Lighting"));
    assert_eq!(attrs.meta_value("icon"), Some("bulb"));
    assert_eq!(attrs.meta_value("order"), Some("2"));
    assert_eq!(attrs.version, 3);
    assert_eq!(type_info::<Generic<u8>>().attrs.version, 0);
}

/// Variants.
#[derive(Reflect, Debug, PartialEq)]
enum Mode {
    /// Off entirely.
    Off,
    #[reflect(display_name = "Always On", doc = "Never sleeps.")]
    On,
}

#[test]
fn variant_metadata() {
    let variants = type_info::<Mode>().variants();
    assert_eq!(variants[0].doc, Some("Off entirely."));
    assert_eq!(variants[0].display_name, None);
    assert_eq!(variants[1].display_name, Some("Always On"));
    assert_eq!(variants[1].doc, Some("Never sleeps."));
    assert_eq!(type_info::<Mode>().attrs.doc, Some("Variants."));
}

#[test]
fn trait_casts() {
    let mut light = Light {
        intensity: 2.0,
        ..Light::default()
    };
    let r = Ref::new(&light);
    let d: &dyn Describe = r.cast::<dyn Describe>().unwrap();
    assert_eq!(d.describe(), "light at 2 EV");
    // A trait the type does not register.
    assert!(r.cast::<dyn std::fmt::Display>().is_none());
    // A type without casts.
    assert!(Ref::new(&1u8).cast::<dyn Describe>().is_none());

    Mut::new(&mut light).cast::<dyn Describe>().unwrap().bump();
    assert_eq!(light.intensity, 3.0);

    let data = type_info::<Light>()
        .data::<ReflectTrait<dyn Describe>>()
        .unwrap();
    assert!(data.get(Ref::new(&light)).is_some());
    // The cast checks the type: the data of `Light` does not apply to another value.
    assert!(data.get(Ref::new(&5u32)).is_none());
}

/// Generic types register casts for every instantiation.
#[derive(Reflect, Debug, PartialEq)]
#[reflect(impl_trait(Describe), data(Marker("generic")))]
struct Generic<T> {
    value: T,
}

impl<T: Reflect> Describe for Generic<T> {
    fn describe(&self) -> String {
        "generic".into()
    }

    fn bump(&mut self) {}
}

/// Type data for `data(...)`.
#[derive(Debug, PartialEq)]
struct Marker(&'static str);

#[test]
fn generic_casts_and_data() {
    let g = Generic { value: 7u16 };
    assert_eq!(
        Ref::new(&g).cast::<dyn Describe>().unwrap().describe(),
        "generic"
    );
    assert_eq!(
        type_info::<Generic<u16>>().data::<Marker>(),
        Some(&Marker("generic"))
    );
    assert_eq!(Ref::new(&g).data::<Marker>(), Some(&Marker("generic")));
    assert!(type_info::<Light>().data::<Marker>().is_none());
}

#[derive(Reflect, Debug, PartialEq)]
struct View<'a, T: 'a> {
    title: Cow<'a, str>,
    items: Vec<T>,
}

#[derive(Reflect, Debug, PartialEq)]
enum Text<'a> {
    Borrowed(Cow<'a, str>),
    Empty,
}

#[test]
fn lifetimes_reflect_the_static_instantiation() {
    let info = type_info::<View<'static, u8>>();
    assert!(info.name.to_string().ends_with("View<u8>"));
    assert!(matches!((info.fields()[0].ty)().kind, TypeKind::String(_)));
    let view = View {
        title: Cow::Borrowed("hello"),
        items: vec![1u8, 2],
    };
    assert_eq!(
        Ref::new(&view).field("title").unwrap().as_str(),
        Some("hello")
    );
    let mut text = Text::Borrowed(Cow::Borrowed("a"));
    assert!(Mut::new(&mut text).field("0").unwrap().set_str("b"));
    assert_eq!(text, Text::Borrowed(Cow::Owned("b".into())));
    assert_eq!(
        Text::reflect_default(),
        Some(Text::Borrowed(Cow::Borrowed("")))
    );
}

#[derive(Reflect, Debug, PartialEq)]
#[reflect(name = "test::Grid")]
struct Grid<T, const W: usize, const SIGNED: i8, const FLAG: bool, const C: char> {
    cells: [T; W],
}

#[test]
fn const_generics_are_part_of_the_name() {
    type G = Grid<f32, 3, -2, true, 'x'>;
    let info = type_info::<G>();
    let name = "test::Grid<f32, 3, -2, true, 'x'>";
    assert_eq!(info.name.to_string(), name);
    assert_eq!(info.id, TypeId::of_name(name));
    assert_ne!(info.id, type_info::<Grid<f32, 4, -2, true, 'x'>>().id);
    let TypeName::Path { args, .. } = info.name else {
        panic!()
    };
    assert!(matches!(args[1], TypeName::Const(ConstArg::Uint(3))));
    assert!(matches!(args[2], TypeName::Const(ConstArg::Int(-2))));
    let grid = G::reflect_default().unwrap();
    assert_eq!(grid.cells, [0.0; 3]);
}

mod reexported {
    pub mod engine {
        pub use marlin_reflect as reflect;
    }

    /// Metadata and casts also work through `crate = "..."`.
    #[derive(engine::reflect::Reflect, Default)]
    #[reflect(crate = "engine::reflect", impl_trait(super::Describe))]
    pub struct Thing {
        #[reflect(widget = Drag)]
        pub x: f32,
    }

    impl super::Describe for Thing {
        fn describe(&self) -> String {
            "thing".into()
        }

        fn bump(&mut self) {}
    }
}

#[test]
fn crate_override_with_metadata() {
    let info = type_info::<reexported::Thing>();
    assert_eq!(info.fields()[0].attrs.widget, Some(Widget::Drag));
    assert_eq!(
        info.attrs.doc,
        Some("Metadata and casts also work through `crate = \"...\"`.")
    );
    let thing = reexported::Thing::default();
    assert_eq!(
        Ref::new(&thing).cast::<dyn Describe>().unwrap().describe(),
        "thing"
    );
}
