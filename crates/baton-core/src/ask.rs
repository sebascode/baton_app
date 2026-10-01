//! Lógica pura de los helpers para scripts (`baton select`, `multiselect`, `confirm`, `input`):
//! de qué variable de entorno sale una respuesta sin terminal y cómo se validan los valores.

use crate::slug::slug;

/// `BATON_<NOMBRE>`: de `--name` o, si no se dio, del texto de la pregunta
/// (`¿Qué instalar?` da `BATON_QUE_INSTALAR`).
pub fn env_var_name(name: Option<&str>, prompt: &str) -> String {
    let base = slug(name.unwrap_or(prompt), "valor");
    format!("BATON_{}", base.to_ascii_uppercase().replace('-', "_"))
}

/// `value` debe ser una de las opciones (sin importar mayúsculas si no hay ambigüedad).
pub fn pick_one(value: &str, options: &[String]) -> Result<String, String> {
    let value = value.trim();
    if let Some(o) = options.iter().find(|o| o.as_str() == value) {
        return Ok(o.clone());
    }
    let folded: Vec<&String> = options
        .iter()
        .filter(|o| o.eq_ignore_ascii_case(value))
        .collect();
    match folded.as_slice() {
        [only] => Ok((*only).clone()),
        _ => Err(format!(
            "'{value}' no es una de las opciones ({})",
            options.join(", ")
        )),
    }
}

/// Varias opciones separadas por coma, en el orden en que están en `options` y sin repetir.
pub fn pick_many(csv: &str, options: &[String]) -> Result<Vec<String>, String> {
    let mut chosen: Vec<String> = Vec::new();
    for part in csv.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let one = pick_one(part, options)?;
        if !chosen.contains(&one) {
            chosen.push(one);
        }
    }
    chosen.sort_by_key(|c| options.iter().position(|o| o == c));
    Ok(chosen)
}

/// `s`, `si`, `sí`, `y`, `yes`, `true`, `1` / `n`, `no`, `false`, `0`.
pub fn parse_yes_no(text: &str) -> Option<bool> {
    match text.trim().to_lowercase().as_str() {
        "s" | "si" | "sí" | "y" | "yes" | "true" | "1" => Some(true),
        "n" | "no" | "false" | "0" => Some(false),
        _ => None,
    }
}

/// Las opciones deben ser al menos una y no repetirse.
pub fn check_options(options: &[String]) -> Result<(), String> {
    if options.is_empty() {
        return Err("faltan las opciones a elegir".to_string());
    }
    for (i, o) in options.iter().enumerate() {
        if o.is_empty() {
            return Err("una opción no puede estar vacía".to_string());
        }
        if options[..i].contains(o) {
            return Err(format!("la opción '{o}' está repetida"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_variable_comes_from_the_name_or_the_question() {
        assert_eq!(env_var_name(None, "¿Ambiente?"), "BATON_AMBIENTE");
        assert_eq!(env_var_name(None, "¿Qué instalar?"), "BATON_QUE_INSTALAR");
        assert_eq!(env_var_name(Some("env"), "¿Ambiente?"), "BATON_ENV");
        assert_eq!(env_var_name(Some("mi-nombre x"), "p"), "BATON_MI_NOMBRE_X");
        assert_eq!(env_var_name(None, "¿¿??"), "BATON_VALOR");
    }

    #[test]
    fn pick_one_accepts_only_known_options() {
        let o = opts(&["dev", "staging", "prod"]);
        assert_eq!(pick_one("prod", &o).unwrap(), "prod");
        assert_eq!(
            pick_one(" Prod ", &o).unwrap(),
            "prod",
            "sin importar mayúsculas"
        );
        let err = pick_one("qa", &o).unwrap_err();
        assert!(
            err.contains("'qa'") && err.contains("dev, staging, prod"),
            "{err}"
        );
        // dos opciones que solo difieren en mayúsculas: hay que ser exacto
        let amb = opts(&["A", "a"]);
        assert_eq!(pick_one("a", &amb).unwrap(), "a");
        assert!(pick_one("A ", &amb).is_ok());
        assert!(pick_one("b", &amb).is_err());
    }

    #[test]
    fn pick_many_keeps_the_option_order_and_drops_repeats() {
        let o = opts(&["postgres", "redis", "nginx"]);
        assert_eq!(
            pick_many("nginx, postgres,nginx", &o).unwrap(),
            ["postgres", "nginx"]
        );
        assert!(pick_many("", &o).unwrap().is_empty());
        assert!(
            pick_many("redis,mongo", &o)
                .unwrap_err()
                .contains("'mongo'")
        );
    }

    #[test]
    fn yes_no_understands_spanish_and_english() {
        for yes in ["s", "SI", "sí", "y", "Yes", "true", "1"] {
            assert_eq!(parse_yes_no(yes), Some(true), "{yes}");
        }
        for no in ["n", "No", "false", "0"] {
            assert_eq!(parse_yes_no(no), Some(false), "{no}");
        }
        assert_eq!(parse_yes_no("quizás"), None);
        assert_eq!(parse_yes_no(""), None);
    }

    #[test]
    fn options_must_exist_and_be_unique() {
        assert!(check_options(&opts(&["a", "b"])).is_ok());
        assert!(check_options(&[]).unwrap_err().contains("faltan"));
        assert!(
            check_options(&opts(&["a", "a"]))
                .unwrap_err()
                .contains("repetida")
        );
        assert!(
            check_options(&opts(&["a", ""]))
                .unwrap_err()
                .contains("vacía")
        );
    }
}
