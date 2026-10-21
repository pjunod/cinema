//! Domain-separated bounded canonical selected-recipe binding.
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::invalid;
use crate::error::StoreError;

fn canonical(value: &Value, output: &mut String) -> Result<(), StoreError> {
    match value {
        Value::Object(object) => {
            output.push('{');
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).map_err(|_| invalid())?);
                output.push(':');
                canonical(&object[key], output)?;
            }
            output.push('}');
        }
        Value::Array(array) => {
            output.push('[');
            for (index, value) in array.iter().enumerate() {
                if index != 0 {
                    output.push(',');
                }
                canonical(value, output)?;
            }
            output.push(']');
        }
        _ => output.push_str(&serde_json::to_string(value).map_err(|_| invalid())?),
    }
    Ok(())
}

/// This is a selected WORKER recipe, not the immutable ingress intent. Object
/// keys are sorted recursively; JSON number spelling is serde_json's canonical
/// parsed representation. The full envelope remains limited to session bounds.
pub fn selected_recipe_digest(recipe: &str) -> Result<String, StoreError> {
    if recipe.len() > 32 * 1024 {
        return Err(invalid());
    }
    let value: Value = serde_json::from_str(recipe).map_err(|_| invalid())?;
    let mut bytes = String::new();
    canonical(&value, &mut bytes)?;
    let mut hasher = Sha256::new();
    hasher.update(b"plurx/adaptive-selected-recipe/v1\0");
    hasher.update(bytes.as_bytes());
    Ok(hex::encode(hasher.finalize()))
}
