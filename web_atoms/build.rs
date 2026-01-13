// Copyright 2014-2017 The html5ever Project Developers. See the
// COPYRIGHT file at the top-level directory of this distribution.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate phf_codegen;
extern crate string_cache_codegen;

use std::collections::BTreeMap;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

mod entities;

static NAMESPACES: &[(&str, &str)] = &[
    ("", ""),
    ("*", "*"),
    ("html", "http://www.w3.org/1999/xhtml"),
    ("xml", "http://www.w3.org/XML/1998/namespace"),
    ("xmlns", "http://www.w3.org/2000/xmlns/"),
    ("xlink", "http://www.w3.org/1999/xlink"),
    ("svg", "http://www.w3.org/2000/svg"),
    ("mathml", "http://www.w3.org/1998/Math/MathML"),
];

fn main() {
    let generated = Path::new(&env::var("OUT_DIR").unwrap()).join("generated.rs");
    let mut generated = BufWriter::new(File::create(generated).unwrap());

    named_entities_to_phf(&Path::new(&env::var("OUT_DIR").unwrap()).join("named_entities.rs"));

    // Read local names from file
    let local_names_path =
        Path::new(&env::var("CARGO_MANIFEST_DIR").unwrap()).join("local_names.txt");
    let local_names: Vec<String> = BufReader::new(File::open(&local_names_path).unwrap())
        .lines()
        .map(|l| l.unwrap())
        .collect();

    // Create a string cache for local names
    let mut local_names_atom = string_cache_codegen::AtomType::new("LocalName", "local_name!");
    for local_name in &local_names {
        local_names_atom.atom(local_name);
        local_names_atom.atom(&local_name.to_ascii_lowercase());
    }
    local_names_atom
        .with_macro_doc("Takes a local name as a string and returns its key in the string cache.")
        .write_to(&mut generated)
        .unwrap();

    // Create a string cache for namespace prefixes
    string_cache_codegen::AtomType::new("Prefix", "namespace_prefix!")
        .with_macro_doc("Takes a namespace prefix string and returns its key in a string cache.")
        .atoms(NAMESPACES.iter().map(|&(prefix, _url)| prefix))
        .write_to(&mut generated)
        .unwrap();

    // Create a string cache for namespace urls
    string_cache_codegen::AtomType::new("Namespace", "namespace_url!")
        .with_macro_doc("Takes a namespace url string and returns its key in a string cache.")
        .atoms(NAMESPACES.iter().map(|&(_prefix, url)| url))
        .write_to(&mut generated)
        .unwrap();

    writeln!(
        generated,
        r#"
        /// Maps the input of [`namespace_prefix!`](macro.namespace_prefix.html) to
        /// the output of [`namespace_url!`](macro.namespace_url.html).
        ///
        #[macro_export] macro_rules! ns {{
        "#
    )
    .unwrap();
    for &(prefix, url) in NAMESPACES {
        writeln!(
            generated,
            "({prefix}) => {{ $crate::namespace_url!({url:?}) }};"
        )
        .unwrap();
    }
    writeln!(generated, "}}").unwrap();

    // Generate C++ files if BLITZ_CPP_OUTPUT_DIR is set (passed from CMake via Corrosion)
    // This outputs to ${CMAKE_BINARY_DIR}/dom when building with CMake
    if let Ok(cpp_output_dir) = env::var("BLITZ_CPP_OUTPUT_DIR") {
        let cpp_output_path = Path::new(&cpp_output_dir);
        std::fs::create_dir_all(&cpp_output_path).ok();
        generate_cpp_local_names(&local_names, cpp_output_path);
        generate_cpp_namespaces(NAMESPACES, cpp_output_path);

        // Tell cargo to rerun if the output dir changes
        println!("cargo:rerun-if-env-changed=BLITZ_CPP_OUTPUT_DIR");
    }
}


/// Convert a kebab-case or lowercase string to PascalCase for C++ enum variants
fn to_pascal_case(s: &str) -> String {
    // Handle custom titlecase mappings first
    match s {
        "role" => return "AccessibilityRole".to_string(),
        "aria-label" => return "AccessibilityTitle".to_string(),
        "aria-description" => return "AccessibilityDescription".to_string(),
        "aria-details" => return "AccessibilityHelpText".to_string(),
        "data-testid" => return "DataTestId".to_string(),
        "uilayoutstype" => return "Uilayoutstype".to_string(),
        "onupdate:modelvalue" => return "ModelValueUpdate".to_string(),
        _ => {}
    }

    let mut result = String::new();
    let mut capitalize_next = true;

    for c in s.chars() {
        if c == '-' || c == ':' || c == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }

    // Handle special cases where the name starts with a digit or is a C++ keyword
    if result
        .chars()
        .next()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false)
    {
        result = format!("_{}", result);
    }

    result
}

/// Check if a string is a valid C++ identifier
fn is_valid_cpp_identifier(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let first = s.chars().next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return false;
    }
    s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Generate C++ header file for Attribute atoms
/// String lookups are handled via Rust FFI in blitz-bindings
fn generate_cpp_local_names(local_names: &[String], output_dir: &Path) {
    // Deduplicate and collect unique local names with their PascalCase variants
    let mut seen = std::collections::HashSet::new();
    let mut entries: Vec<(String, String)> = Vec::new();

    for name in local_names {
        let lower = name.to_ascii_lowercase();
        // Skip empty strings and invalid entries
        if lower.is_empty() {
            continue;
        }
        if seen.insert(lower.clone()) {
            let pascal = to_pascal_case(&lower);
            // Skip entries that result in invalid C++ identifiers
            if !is_valid_cpp_identifier(&pascal) {
                continue;
            }
            entries.push((lower, pascal));
        }
    }

    // Sort for consistent output
    entries.sort_by(|a, b| a.1.cmp(&b.1));

    // Generate header file (enum only)
    let header_path = output_dir.join("ui_attributes.h");
    let mut header = BufWriter::new(File::create(header_path).unwrap());

    writeln!(
        header,
        r#"// Auto-generated by web_atoms build.rs - DO NOT EDIT
#pragma once

#include <cstdint>

namespace ui::dom {{

/// Enum representing HTML/SVG attribute and element names.
/// These values correspond to Blitz's internal LocalName atoms.
/// String conversions are done via Rust FFI (blitz::attribute_to_string, etc.)
enum class Attribute : uint16_t
{{"#
    )
    .unwrap();

    for (i, (_name, pascal)) in entries.iter().enumerate() {
        writeln!(header, "    {} = {},", pascal, i).unwrap();
    }

    writeln!(
        header,
        r#"}};

constexpr size_t attributeCount = {};

}} // namespace ui::dom"#,
        entries.len()
    )
    .unwrap();
}

/// Generate C++ header file for Namespace atoms
/// String lookups are handled via Rust FFI in blitz-bindings
fn generate_cpp_namespaces(namespaces: &[(&str, &str)], output_dir: &Path) {
    // Collect namespace entries, filtering out invalid C++ identifiers
    let entries: Vec<(&str, &str, String)> = namespaces
        .iter()
        .filter(|(prefix, _)| !prefix.is_empty())
        .map(|(prefix, url)| {
            let pascal = to_pascal_case(prefix);
            (*prefix, *url, pascal)
        })
        .filter(|(_, _, pascal)| is_valid_cpp_identifier(pascal))
        .collect();

    // Generate header file (enum only)
    let header_path = output_dir.join("ui_namespaces.h");
    let mut header = BufWriter::new(File::create(header_path).unwrap());

    writeln!(
        header,
        r#"// Auto-generated by web_atoms build.rs - DO NOT EDIT
#pragma once

#include <cstdint>

namespace ui::dom {{

/// Enum representing XML namespace IDs.
/// These values correspond to Blitz's internal namespace atoms.
/// String conversions are done via Rust FFI (blitz::namespace_to_prefix, etc.)
enum class NamespaceID : uint8_t
{{
    None = 0,"#
    )
    .unwrap();

    for (i, (_, _, pascal)) in entries.iter().enumerate() {
        writeln!(header, "    {} = {},", pascal, i + 1).unwrap();
    }

    writeln!(
        header,
        r#"}};

constexpr size_t namespaceCount = {};

}} // namespace ui::dom"#,
        entries.len() + 1
    )
    .unwrap();
}

fn named_entities_to_phf(to: &Path) {
    let mut entities: BTreeMap<&str, (u32, u32)> = entities::NAMED_ENTITIES
        .iter()
        .map(|(name, cp1, cp2)| {
            assert!(name.starts_with('&'));
            (&name[1..], (*cp1, *cp2))
        })
        .collect();

    // Add every missing prefix of those keys, mapping to NULL characters.
    for key in entities.keys().cloned().collect::<Vec<_>>() {
        for n in 1..key.len() {
            entities.entry(&key[..n]).or_insert((0, 0));
        }
    }
    entities.insert("", (0, 0));

    let mut phf_map = phf_codegen::Map::new();
    for (key, value) in entities {
        phf_map.entry(key, format!("{value:?}"));
    }

    let mut file = File::create(to).unwrap();
    writeln!(
        &mut file,
        r#"
/// A map of entity names to their codepoints. The second codepoint will
/// be 0 if the entity contains a single codepoint. Entities have their preceding '&' removed.
///
/// # Examples
///
/// ```
/// use web_atoms::NAMED_ENTITIES;
///
/// assert_eq!(NAMED_ENTITIES.get("gt;").unwrap(), &(62, 0));
/// ```
"#
    )
    .unwrap();
    writeln!(
        &mut file,
        "pub static NAMED_ENTITIES: Map<&'static str, (u32, u32)> = {};",
        phf_map.build(),
    )
    .unwrap();
}
