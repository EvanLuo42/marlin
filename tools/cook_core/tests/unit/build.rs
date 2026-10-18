//! Build dependencies (0003 §4.1): objects a cooker reads besides its own, whose units' keys are
//! part of the dependent's key and which are not imports.

use marlin_asset::cooked::{Encoding, UnitView};
use marlin_asset::{Link, SourceFile, Uuid};
use marlin_cook_core::{
    CookContext, CookOptions, Cooked, Cooker, Cookers, Platform, Profile, SourceObject, cook_all,
    unit_key,
};
use marlin_reflect::{Reflect, type_info};

use crate::support::{Project, uuid};

/// Built from other recipes; refers to a [`Garnish`] at run time.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Recipe {
    source: SourceFile,
    inputs: Vec<Link<Recipe>>,
    garnish: Link<Garnish>,
    /// Asks the cook context for the garnish, which is not a build dependency.
    peek: bool,
}

#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Garnish {
    value: u32,
}

/// The cooked recipe: its source and, depth first, its inputs' sources.
#[derive(Clone, PartialEq, Debug, Default, Reflect)]
struct Dish {
    text: String,
}

struct RecipeCooker;

/// `recipe`'s source, then its inputs', depth first.
fn gather(recipe: &Recipe, ctx: &mut CookContext<'_>, out: &mut String) -> Result<(), String> {
    out.push_str(&String::from_utf8(ctx.read_source(&recipe.source.path)?).unwrap());
    for input in &recipe.inputs {
        let dependency = ctx.dependency(input.target)?;
        let inner: &Recipe = dependency.value.as_ref().downcast().ok_or("not a Recipe")?;
        gather(inner, ctx, out)?;
    }
    Ok(())
}

impl Cooker for RecipeCooker {
    fn version(&self) -> u32 {
        1
    }

    fn build_dependency_types(&self) -> &[&str] {
        &["Recipe"]
    }

    fn cook(&self, object: &SourceObject<'_>, ctx: &mut CookContext<'_>) -> Result<Cooked, String> {
        let recipe: &Recipe = object.value.as_ref().downcast().ok_or("not a Recipe")?;
        if recipe.peek {
            ctx.dependency(recipe.garnish.target)?;
        }
        let mut text = String::new();
        gather(recipe, ctx, &mut text)?;
        Ok(Cooked {
            type_id: type_info::<Dish>().id,
            encoding: Encoding::Serial,
            payload: marlin_serial::to_bytes(&Dish { text }).map_err(|e| format!("{e:?}"))?,
        })
    }
}

fn cookers() -> Cookers {
    let mut c = Cookers::new();
    c.register::<Recipe>(RecipeCooker).unwrap();
    c.register_generic::<Garnish>().unwrap();
    c
}

const SOUP: Uuid = uuid(1);
const STOCK: Uuid = uuid(2);
const SALT: Uuid = uuid(3);
const PARSLEY: Uuid = uuid(4);
const BREAD: Uuid = uuid(5);

/// `<name>.masset` holding the recipe `name` (object `id`) with `source` and `inputs`.
fn recipe(p: &Project, name: &str, id: Uuid, inputs: &[(&str, Uuid)], extra: &str) {
    let inputs: Vec<String> = inputs
        .iter()
        .map(|(n, u)| format!("<{n}:{n} #{u}>"))
        .collect();
    p.write(&format!("{name}.txt"), name);
    p.write(
        &format!("{name}.masset"),
        format!(
            "!Version 1\nRecipe {{ !Uuid = \"{id}\" !Name = \"{name}\" Source = `source:{name}.txt` \
             Inputs = [{}] {extra} }}\n",
            inputs.join(", ")
        ),
    );
}

/// soup ← stock ← salt (build dependencies); soup refers to parsley at run time; bread stands
/// alone.
fn kitchen(p: &Project) {
    recipe(p, "salt", SALT, &[], "");
    recipe(p, "stock", STOCK, &[("salt", SALT)], "");
    recipe(
        p,
        "soup",
        SOUP,
        &[("stock", STOCK)],
        &format!("Garnish = <parsley:Parsley #{PARSLEY}>"),
    );
    recipe(p, "bread", BREAD, &[], "");
    p.write(
        "parsley.masset",
        format!("!Version 1\nGarnish {{ !Uuid = \"{PARSLEY}\" !Name = \"Parsley\" Value = 7 }}\n"),
    );
}

fn key(p: &Project, path: &str) -> u128 {
    let db = p.db();
    let file = db.file(path).unwrap();
    unit_key(
        &db,
        &cookers(),
        file,
        Platform::host(),
        Profile::Development,
    )
    .unwrap()
}

#[test]
fn a_cooker_reads_its_build_dependencies_and_their_sources() {
    let p = Project::new("build-read");
    kitchen(&p);
    let units = p.cook(&cookers()).unwrap();
    let soup = units.iter().find(|u| u.path == "soup").unwrap();
    let view = UnitView::new(&soup.bytes).unwrap();
    let (_, entry) = view.find(SOUP).unwrap();
    let dish: Dish = marlin_serial::from_bytes(view.payload(&entry)).unwrap();
    assert_eq!(
        dish.text, "soupstocksalt",
        "transitively, through the stock"
    );

    // The runtime reference is an import; what the soup is built from is not.
    assert_eq!(view.import_count(), 1);
    assert_eq!(view.import(0).unwrap().uuid, PARSLEY);
}

#[test]
fn a_dependency_s_inputs_are_in_its_dependents_keys() {
    let p = Project::new("build-keys");
    kitchen(&p);
    let before: Vec<u128> = ["soup", "stock", "salt", "bread"]
        .iter()
        .map(|f| key(&p, &format!("{f}.masset")))
        .collect();

    // The salt's source file: everything built from it, transitively, cooks again.
    p.write("salt.txt", "more salt");
    let after: Vec<u128> = ["soup", "stock", "salt", "bread"]
        .iter()
        .map(|f| key(&p, &format!("{f}.masset")))
        .collect();
    assert_ne!(after[0], before[0]);
    assert_ne!(after[1], before[1]);
    assert_ne!(after[2], before[2]);
    assert_eq!(after[3], before[3], "not built from the salt");

    // A runtime reference's content is not an input.
    let soup = key(&p, "soup.masset");
    p.write(
        "parsley.masset",
        format!("!Version 1\nGarnish {{ !Uuid = \"{PARSLEY}\" !Name = \"Parsley\" Value = 8 }}\n"),
    );
    assert_eq!(key(&p, "soup.masset"), soup);

    // The whole cook computes the same keys, sharing the dependencies'.
    let db = p.db();
    let (report, _) = cook_all(&db, &cookers(), &p.ddc("ddc"), &CookOptions::default());
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    for unit in &report.units {
        assert_eq!(
            unit.key,
            key(&p, &format!("{}.masset", unit.path)),
            "{}",
            unit.path
        );
    }
}

#[test]
fn build_dependency_cycles_between_units_are_errors() {
    let p = Project::new("build-cycle");
    recipe(&p, "egg", uuid(10), &[("hen", uuid(11))], "");
    recipe(&p, "hen", uuid(11), &[("egg", uuid(10))], "");
    recipe(&p, "toast", uuid(12), &[("egg", uuid(10))], "");
    let errors = p.cook(&cookers()).unwrap_err();
    assert_eq!(errors.len(), 3, "{errors:?}");
    for e in &errors {
        assert!(e.contains("form a cycle"), "{e}");
    }
    let toast = errors
        .iter()
        .find(|e| e.starts_with("toast.masset"))
        .unwrap();
    assert!(
        toast.contains("egg.masset → hen.masset → egg.masset"),
        "{toast}"
    );
}

#[test]
fn only_build_dependencies_can_be_read() {
    let p = Project::new("build-refuse");
    kitchen(&p);
    recipe(
        &p,
        "soup",
        SOUP,
        &[("stock", STOCK)],
        &format!("Garnish = <parsley:Parsley #{PARSLEY}> Peek = true"),
    );
    let errors = p.cook(&cookers()).unwrap_err();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("not a build dependency"),
        "{}",
        errors[0]
    );
}

#[test]
fn a_build_dependency_that_does_not_cook_fails_its_dependents() {
    let p = Project::new("build-broken");
    kitchen(&p);
    p.write(
        "salt.masset",
        format!(
            "!Version 1\nRecipe {{ !Uuid = \"{SALT}\" !Name = \"salt\" Source = `source:gone.txt` }}\n"
        ),
    );
    let errors = p.cook(&cookers()).unwrap_err();
    assert_eq!(errors.len(), 3, "salt, stock and soup: {errors:?}");
    let soup = errors
        .iter()
        .find(|e| e.starts_with("soup.masset"))
        .unwrap();
    assert!(soup.contains("its build dependency"), "{soup}");
    assert!(soup.contains("gone.txt"), "{soup}");

    // A link to an object nothing defines.
    recipe(&p, "salt", SALT, &[], "");
    recipe(&p, "stock", STOCK, &[("salt", uuid(99))], "");
    let errors = p.cook(&cookers()).unwrap_err();
    assert!(
        errors.iter().all(|e| e.contains("no indexed file defines")),
        "{errors:?}"
    );
}
