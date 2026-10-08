//! Manifiesto de un plugin (`baton-plugin.toml`, `api = 1`): un tipo de paso nuevo descrito con
//! datos, sin código. Este módulo solo lee y valida el texto y lo convierte en un [`KindSpec`];
//! encontrar los archivos en disco es cosa de `baton-store`.
//!
//! ```toml
//! api = 1
//! name = "terraform"
//! version = "0.1.0"
//! description = "Terraform por carpeta"
//! requires = ["terraform"]            # programas que tienen que existir donde corre el paso
//!
//! [type]
//! name = "terraform"                  # opcional: por defecto, el nombre del plugin
//! scanned = true                      # el origen es un glob y corre una vez por archivo
//! detect = ["**/main.tf"]             # lo que `baton init` propone
//! command = "terraform init -input=false && terraform apply -input=false -auto-approve"
//! dry_run = "terraform init -input=false && terraform plan -input=false"
//! destructive = ["will be destroyed", "must be replaced"]   # frases del plan que piden confirmar
//!
//! # Credenciales: un tipo nuevo (con sus campos) y cómo llegan a los comandos del plugin
//! [[credentials]]
//! kind = "aws"
//! fields = [
//!   { key = "ACCESS_KEY_ID" },
//!   { key = "SECRET_ACCESS_KEY", secret = true },
//!   { key = "REGION", optional = true },
//! ]
//! [credentials.env]
//! AWS_ACCESS_KEY_ID = "{ACCESS_KEY_ID}"
//! AWS_SECRET_ACCESS_KEY = "{SECRET_ACCESS_KEY}"
//! AWS_DEFAULT_REGION = "{REGION}"
//! ```
//!
//! Un tipo de credencial se define por sus campos y vale para cualquier plugin; las variables de
//! entorno (`[credentials.env]`) son de cada plugin, porque cada herramienta espera las suyas. Un
//! plugin también puede usar un tipo de baton (`docker`, `git`...) sin `fields`, solo con su `env`.
//!
//! Un plugin solo describe qué comando correr: la ejecución, las credenciales, los logs y los
//! gates siguen siendo de baton. Por eso el manifiesto no puede pedir backup ni gate, ni redefinir
//! un tipo nativo.

use serde::Deserialize;

use crate::credential::FieldSpec;
use crate::credential_kind::{self, CredentialKind, PluginCredential};
use crate::issue::Issue;
use crate::kind::{self, CredUse, KindSpec, Requires, StepKind};
use crate::path;
use crate::template::{STEP_VARS, unknown_placeholders};
use crate::validate::unsafe_source;

/// Versión del formato del manifiesto que entiende esta versión de baton.
pub const API: u32 = 1;
/// Nombre del archivo dentro de la carpeta del plugin.
pub const MANIFEST_FILE: &str = "baton-plugin.toml";
/// Un manifiesto más grande que esto no se lee: es texto corto, no un paquete.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

const MAX_COMMAND: usize = 4096;
const MAX_DETECT: usize = 20;
const MAX_DESTRUCTIVE: usize = 20;
const MIN_PHRASE: usize = 4;
const MAX_PHRASE: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub api: u32,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Programas que tienen que existir donde corre el paso.
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(rename = "type")]
    pub step_type: TypeSection,
    /// Credenciales que usa el tipo y cómo las recibe su comando.
    #[serde(default)]
    pub credentials: Vec<CredentialDecl>,
}

/// Una credencial que usa el plugin.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialDecl {
    /// El nombre del tipo (`aws`). Si no es de baton, `fields` lo define.
    pub kind: String,
    /// Los campos de un tipo nuevo. No vale con un tipo de baton: ya tiene los suyos.
    #[serde(default)]
    pub fields: Vec<FieldDecl>,
    /// `VARIABLE = "plantilla con {CAMPOS}"`: lo que recibe el comando. Una variable cuya plantilla
    /// usa un campo opcional que quedó vacío no se define.
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDecl {
    /// `ACCESS_KEY_ID`: mayúsculas, números y guiones bajos.
    pub key: String,
    /// Etiqueta del formulario; por defecto, la clave en minúsculas.
    pub label: Option<String>,
    /// Se enmascara en pantalla y se tacha de todo lo que se muestra o guarda.
    #[serde(default)]
    pub secret: bool,
    /// Puede quedar vacío.
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeSection {
    /// Cómo se escribe en el plan (`type = "..."`). Por defecto, el nombre del plugin.
    pub name: Option<String>,
    #[serde(default)]
    pub scanned: bool,
    #[serde(default)]
    pub detect: Vec<String>,
    /// Comando por defecto del tipo (el paso puede declarar el suyo).
    pub command: String,
    /// Comando de solo lectura para `--dry-run`.
    pub dry_run: Option<String>,
    /// Frases que, si aparecen en la salida del `dry_run`, indican que el paso destruye o reemplaza
    /// algo. Antes de ejecutar el paso, baton corre el `dry_run`, las busca (sin distinguir
    /// mayúsculas ni colores) y pide confirmación. Texto literal, no expresiones regulares.
    #[serde(default)]
    pub destructive: Vec<String>,
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, toml::de::Error> {
        toml::from_str(text)
    }

    /// El `api` declarado, aunque el resto del archivo no se entienda. Sirve para explicar que un
    /// manifiesto es de una versión del formato que esta baton no conoce, en vez de quejarse del
    /// primer campo nuevo.
    pub fn declared_api(text: &str) -> Option<u32> {
        #[derive(Deserialize)]
        struct Only {
            api: Option<u32>,
        }
        toml::from_str::<Only>(text).ok()?.api
    }

    /// Nombre del tipo de paso que define.
    pub fn type_name(&self) -> &str {
        self.step_type.name.as_deref().unwrap_or(&self.name)
    }

    /// El descriptor del tipo. Filtra la memoria de sus textos: se registra una vez por proceso y
    /// vive hasta que termina.
    pub fn to_spec(&self) -> KindSpec {
        let t = &self.step_type;
        KindSpec {
            name: leak(self.type_name()),
            scanned: t.scanned,
            has_services: false,
            default_command: Some(leak(t.command.trim())),
            runs_command: true,
            own_interpreter: false,
            requires: if t.scanned {
                Requires::Source
            } else {
                Requires::Command
            },
            dry_run: t.dry_run.as_deref().map(|c| leak(c.trim())),
            detect: leak_all(&t.detect),
            binaries: leak_all(&self.requires),
            destructive: leak_all(&t.destructive),
            credentials: Box::leak(
                self.credentials
                    .iter()
                    .map(|c| CredUse {
                        kind: leak(&c.kind),
                        env: Box::leak(
                            c.env
                                .iter()
                                .map(|(k, v)| (leak(k), leak(v)))
                                .collect::<Vec<_>>()
                                .into_boxed_slice(),
                        ),
                    })
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            ),
        }
    }

    /// Registra sus tipos de credencial nuevos y su tipo de paso, para que los planes puedan
    /// usarlos.
    pub fn register(&self) -> Result<StepKind, String> {
        for c in self.credentials.iter().filter(|c| !c.fields.is_empty()) {
            credential_kind::register(PluginCredential {
                name: leak(&c.kind),
                fields: leak_fields(&c.fields),
            })?;
        }
        kind::register(self.to_spec())
    }
}

fn leak_fields(fields: &[FieldDecl]) -> &'static [FieldSpec] {
    let v: Vec<FieldSpec> = fields
        .iter()
        .map(|f| FieldSpec {
            key: leak(&f.key),
            label: leak(&f.label.clone().unwrap_or_else(|| default_label(&f.key))),
            secret: f.secret,
            optional: f.optional,
        })
        .collect();
    Box::leak(v.into_boxed_slice())
}

/// `ACCESS_KEY_ID` -> `access key id`.
fn default_label(key: &str) -> String {
    key.to_lowercase().replace('_', " ")
}

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn leak_all(items: &[String]) -> &'static [&'static str] {
    let v: Vec<&'static str> = items.iter().map(|s| leak(s)).collect();
    Box::leak(v.into_boxed_slice())
}

/// Palabras que modifican algo (aplicar, borrar, desplegar...). Un `dry_run` que las use no se
/// acepta: tiene que ser de solo lectura.
const MUTATING: &[&str] = &[
    "apply",
    "destroy",
    "delete",
    "create",
    "deploy",
    "rm",
    "up",
    "down",
    "push",
    "publish",
    "-auto-approve",
    "--auto-approve",
];

/// La primera palabra de `command` que parece modificar algo, si la hay.
///
/// Es un chequeo de apariencia, no una garantía: un comando puede esconder lo que hace. La
/// garantía de verdad es que instalar un plugin muestra sus comandos y pide confirmación.
pub fn mutating_word(command: &str) -> Option<&'static str> {
    command
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')'))
        .map(|w| w.trim_matches(['"', '\'']).to_ascii_lowercase())
        .find_map(|w| MUTATING.iter().copied().find(|m| *m == w))
}

/// El texto sin las secuencias de escape ANSI (`ESC [ ... letra`): una herramienta puede pintar
/// de rojo solo una palabra de la frase y partirla.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            // parámetros hasta la letra final (0x40..=0x7e)
            for n in chars.by_ref() {
                if ('@'..='~').contains(&n) {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Las líneas de `output` que contienen alguna de las frases `patterns`, sin colores ANSI y sin
/// distinguir mayúsculas. Devuelve cada línea limpia y recortada.
pub fn destructive_hits(output: &[String], patterns: &[&str]) -> Vec<String> {
    let patterns: Vec<String> = patterns
        .iter()
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    output
        .iter()
        .map(|l| strip_ansi(l))
        .filter(|l| {
            let lower = l.to_lowercase();
            patterns.iter().any(|p| lower.contains(p.as_str()))
        })
        .map(|l| l.trim().to_string())
        .collect()
}

fn is_binary_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('-')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

fn is_plain_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Valida un manifiesto ya leído. Los problemas llevan la ruta dentro del TOML.
pub fn validate_manifest(m: &Manifest) -> Vec<Issue> {
    let mut out = Vec::new();

    if m.api != API {
        out.push(Issue::error(
            path!["api"],
            format!(
                "api = {} no está soportada: esta versión de baton entiende api = {API}",
                m.api
            ),
        ));
    }
    if !kind::is_valid_name(&m.name) {
        out.push(Issue::error(
            path!["name"],
            format!(
                "nombre '{}' no válido: solo minúsculas, números y guiones",
                m.name
            ),
        ));
    }
    if !is_plain_version(&m.version) {
        out.push(Issue::error(
            path!["version"],
            format!(
                "versión '{}' no válida: usa X.Y.Z (por ejemplo 0.1.0)",
                m.version
            ),
        ));
    }
    for (i, r) in m.requires.iter().enumerate() {
        if !is_binary_name(r) {
            out.push(Issue::error(
                path!["requires", i],
                format!("'{r}' no es el nombre de un programa (sin rutas ni espacios)"),
            ));
        }
    }

    let t = &m.step_type;
    let type_name = m.type_name();
    if t.name.is_some() && !kind::is_valid_name(type_name) {
        out.push(Issue::error(
            path!["type", "name"],
            format!("nombre '{type_name}' no válido: solo minúsculas, números y guiones"),
        ));
    }
    if kind::is_valid_name(type_name)
        && StepKind::from_name(type_name).is_some_and(|k| k.is_builtin())
    {
        out.push(Issue::error(
            path!["type", "name"],
            format!("'{type_name}' ya es un tipo de baton y no se puede redefinir"),
        ));
    }

    check_command(&mut out, "command", &t.command);
    if let Some(dry) = &t.dry_run {
        check_command(&mut out, "dry_run", dry);
        if dry.trim() == t.command.trim() {
            out.push(Issue::error(
                path!["type", "dry_run"],
                "dry_run es igual a command: tiene que ser de solo lectura",
            ));
        } else if let Some(word) = mutating_word(dry) {
            out.push(Issue::error(
                path!["type", "dry_run"],
                format!(
                    "dry_run usa '{word}', que modifica algo: un dry-run de plugin es de solo \
                     lectura (plan, what-if, validate...)"
                ),
            ));
        }
    }

    if t.detect.len() > MAX_DETECT {
        out.push(Issue::error(
            path!["type", "detect"],
            format!(
                "demasiados patrones ({}, máximo {MAX_DETECT})",
                t.detect.len()
            ),
        ));
    }
    for (i, pattern) in t.detect.iter().enumerate() {
        let at = path!["type", "detect", i];
        if pattern.trim().is_empty() {
            out.push(Issue::error(at, "el patrón está vacío"));
        } else if let Some(why) = unsafe_source(pattern) {
            out.push(Issue::error(
                at,
                format!("patrón '{pattern}' no permitido: {why}"),
            ));
        } else if let Err(e) = glob::Pattern::new(pattern) {
            out.push(Issue::error(
                at,
                format!("patrón '{pattern}' no válido: {e}"),
            ));
        }
    }
    if !t.destructive.is_empty() && t.dry_run.is_none() {
        out.push(Issue::error(
            path!["type", "destructive"],
            "destructive se busca en la salida del dry_run: declara un dry_run",
        ));
    }
    if t.destructive.len() > MAX_DESTRUCTIVE {
        out.push(Issue::error(
            path!["type", "destructive"],
            format!(
                "demasiadas frases ({}, máximo {MAX_DESTRUCTIVE})",
                t.destructive.len()
            ),
        ));
    }
    for (i, phrase) in t.destructive.iter().enumerate() {
        let len = phrase.trim().chars().count();
        if len < MIN_PHRASE || phrase.chars().count() > MAX_PHRASE {
            out.push(Issue::error(
                path!["type", "destructive", i],
                format!(
                    "la frase debe tener entre {MIN_PHRASE} y {MAX_PHRASE} caracteres: una muy corta \
                     coincidiría con casi cualquier línea"
                ),
            ));
        }
    }
    validate_credentials(m, &mut out);

    if !t.scanned && !t.detect.is_empty() {
        out.push(Issue::warning(
            path!["type", "detect"],
            "detect solo se usa con scanned = true",
        ));
    }

    out
}

/// La plantilla con cada `{CAMPO}` reemplazado por su valor, en una sola pasada (el valor de un
/// campo nunca se interpreta como otro `{CAMPO}`). `None` si falta el valor de algún campo o está
/// vacío: la variable entonces no se define.
pub fn render_env(template: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let close = after.find('}')?;
        let value = lookup(&after[..close]).filter(|v| !v.is_empty())?;
        out.push_str(&rest[..open]);
        out.push_str(&value);
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Some(out)
}

const MAX_CREDENTIALS: usize = 4;
const MAX_FIELDS: usize = 16;
const MAX_ENV: usize = 16;

/// Variables que un plugin no puede definir con una credencial: cambiarlas altera cómo se lanza o
/// se interpreta cualquier comando, y no hacen falta para entregar un secreto.
const FORBIDDEN_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "SHELL",
    "IFS",
    "ENV",
    "BASH_ENV",
    "SHELLOPTS",
    "BASHOPTS",
    "PS4",
    "PROMPT_COMMAND",
    "LD_PRELOAD",
    "LD_LIBRARY_PATH",
    "LD_AUDIT",
];

fn forbidden_env(name: &str) -> bool {
    FORBIDDEN_ENV.contains(&name) || name.starts_with("DYLD_") || name.starts_with("BATON_")
}

fn is_env_name(s: &str) -> bool {
    let mut chars = s.chars();
    s.len() <= 64
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Los `{CAMPO}` de una plantilla de variable. `Err` si hay una llave sin cerrar o un nombre que no
/// es un campo.
pub fn env_placeholders(template: &str) -> Result<Vec<&str>, String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let close = after
            .find('}')
            .ok_or_else(|| "hay una { sin cerrar".to_string())?;
        let name = &after[..close];
        if !credential_kind::is_valid_field_key(name) {
            return Err(format!(
                "{{{name}}} no es un campo (mayúsculas, números y _)"
            ));
        }
        out.push(name);
        rest = &after[close + 1..];
    }
    Ok(out)
}

fn validate_credentials(m: &Manifest, out: &mut Vec<Issue>) {
    if m.credentials.len() > MAX_CREDENTIALS {
        out.push(Issue::error(
            path!["credentials"],
            format!(
                "demasiadas credenciales ({}, máximo {MAX_CREDENTIALS})",
                m.credentials.len()
            ),
        ));
    }
    let mut seen: Vec<&str> = Vec::new();
    for (i, c) in m.credentials.iter().enumerate() {
        let at = |field: &str| path!["credentials", i, field];
        if !kind::is_valid_name(&c.kind) {
            out.push(Issue::error(
                at("kind"),
                format!(
                    "nombre '{}' no válido: solo minúsculas, números y guiones",
                    c.kind
                ),
            ));
            continue;
        }
        if seen.contains(&c.kind.as_str()) {
            out.push(Issue::error(
                at("kind"),
                format!("la credencial '{}' está declarada dos veces", c.kind),
            ));
        }
        seen.push(&c.kind);

        // los campos de los que puede hablar el env: los del tipo de baton o los que declara
        let builtin = CredentialKind::from_name(&c.kind).filter(|k| k.is_builtin());
        let keys: Vec<&str> = match builtin {
            Some(k) => {
                if !c.fields.is_empty() {
                    out.push(Issue::error(
                        at("fields"),
                        format!(
                            "'{}' es un tipo de baton y ya tiene sus campos: quita fields y deja solo env",
                            c.kind
                        ),
                    ));
                }
                crate::credential::fields_for(k)
                    .iter()
                    .map(|f| f.key)
                    .collect()
            }
            None => {
                validate_fields(c, i, out);
                c.fields.iter().map(|f| f.key.as_str()).collect()
            }
        };

        if c.env.is_empty() {
            out.push(Issue::error(
                at("env"),
                "sin env la credencial no llegaría a ningún comando: declara qué variables recibe",
            ));
        }
        if c.env.len() > MAX_ENV {
            out.push(Issue::error(
                at("env"),
                format!("demasiadas variables ({}, máximo {MAX_ENV})", c.env.len()),
            ));
        }
        for (name, template) in &c.env {
            let here = path!["credentials", i, "env", name.as_str()];
            if !is_env_name(name) {
                out.push(Issue::error(
                    here,
                    format!("'{name}' no es un nombre de variable (mayúsculas, números y _)"),
                ));
                continue;
            }
            if forbidden_env(name) {
                out.push(Issue::error(
                    here,
                    format!(
                        "una credencial no puede definir {name}: cambia cómo se lanzan los comandos"
                    ),
                ));
                continue;
            }
            if template.trim().is_empty() || template.len() > 200 {
                out.push(Issue::error(
                    here,
                    "la plantilla no puede estar vacía ni pasar de 200 caracteres",
                ));
                continue;
            }
            match env_placeholders(template) {
                Err(e) => out.push(Issue::error(here, e)),
                Ok(used) if used.is_empty() => out.push(Issue::error(
                    here,
                    "la plantilla no usa ningún {CAMPO}: una credencial entrega valores de la credencial",
                )),
                Ok(used) => {
                    for field in used {
                        if !keys.contains(&field) {
                            out.push(Issue::error(
                                path!["credentials", i, "env", name.as_str()],
                                format!(
                                    "{{{field}}} no es un campo de '{}' (los hay: {})",
                                    c.kind,
                                    keys.join(", ")
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }
}

fn validate_fields(c: &CredentialDecl, i: usize, out: &mut Vec<Issue>) {
    if c.fields.is_empty() {
        out.push(Issue::error(
            path!["credentials", i, "fields"],
            format!(
                "'{}' no es un tipo de baton: declara sus campos con fields",
                c.kind
            ),
        ));
        return;
    }
    if c.fields.len() > MAX_FIELDS {
        out.push(Issue::error(
            path!["credentials", i, "fields"],
            format!(
                "demasiados campos ({}, máximo {MAX_FIELDS})",
                c.fields.len()
            ),
        ));
    }
    let mut keys: Vec<&str> = Vec::new();
    for (j, f) in c.fields.iter().enumerate() {
        let at = path!["credentials", i, "fields", j];
        if !credential_kind::is_valid_field_key(&f.key) {
            out.push(Issue::error(
                at,
                format!(
                    "clave '{}' no válida: mayúsculas, números y _, empezando por letra",
                    f.key
                ),
            ));
        } else if keys.contains(&f.key.as_str()) {
            out.push(Issue::error(
                at,
                format!("el campo {} está repetido", f.key),
            ));
        }
        keys.push(&f.key);
        if f.label
            .as_deref()
            .is_some_and(|l| l.trim().is_empty() || l.len() > 40)
        {
            out.push(Issue::error(
                path!["credentials", i, "fields", j, "label"],
                "la etiqueta no puede estar vacía ni pasar de 40 caracteres",
            ));
        }
    }
}

fn check_command(out: &mut Vec<Issue>, field: &str, command: &str) {
    let at = path!["type", field];
    if command.trim().is_empty() {
        out.push(Issue::error(at, format!("{field} no puede estar vacío")));
        return;
    }
    if command.len() > MAX_COMMAND {
        out.push(Issue::error(
            at,
            format!("{field} es demasiado largo (máximo {MAX_COMMAND} caracteres)"),
        ));
        return;
    }
    for unknown in unknown_placeholders(command, STEP_VARS) {
        out.push(Issue::warning(
            at.clone(),
            format!(
                "placeholder desconocido {{{unknown}}} (disponibles: {}); se deja tal cual",
                STEP_VARS
                    .iter()
                    .map(|v| format!("{{{v}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::has_errors;
    use crate::plan::Plan;

    const TERRAFORM: &str = r#"
api = 1
name = "terraform"
version = "0.1.0"
description = "Terraform por carpeta"
requires = ["terraform"]

[type]
scanned = true
detect = ["**/main.tf"]
command = "terraform init -input=false && terraform apply -input=false -auto-approve"
dry_run = "terraform init -input=false && terraform plan -input=false"
"#;

    fn manifest(body: &str) -> Manifest {
        Manifest::parse(body).unwrap_or_else(|e| panic!("{e}\n{body}"))
    }

    fn errors(m: &Manifest) -> Vec<String> {
        validate_manifest(m)
            .into_iter()
            .filter(Issue::is_error)
            .map(|i| format!("{}: {}", i.path_string(), i.message))
            .collect()
    }

    /// El manifiesto base con un campo cambiado.
    fn with(from: &str, to: &str) -> Manifest {
        assert!(TERRAFORM.contains(from), "{from}");
        manifest(&TERRAFORM.replace(from, to))
    }

    #[test]
    fn a_complete_manifest_is_valid_and_defaults_the_type_name_to_the_plugin_name() {
        let m = manifest(TERRAFORM);
        assert!(
            validate_manifest(&m).is_empty(),
            "{:?}",
            validate_manifest(&m)
        );
        assert_eq!(m.type_name(), "terraform");
        assert_eq!(m.requires, ["terraform"]);
    }

    #[test]
    fn the_type_can_have_its_own_name() {
        let m = with("[type]\n", "[type]\nname = \"tf\"\n");
        assert_eq!(m.type_name(), "tf");
        assert!(errors(&m).is_empty());
    }

    #[test]
    fn unknown_fields_are_a_read_error_so_a_typo_is_not_silently_ignored() {
        assert!(Manifest::parse(&TERRAFORM.replace("scanned", "scaned")).is_err());
        assert!(Manifest::parse(&format!("{TERRAFORM}\nextra = 1\n")).is_err());
    }

    #[test]
    fn missing_required_fields_are_a_read_error() {
        assert!(Manifest::parse("api = 1\nname = \"x\"\nversion = \"0.1.0\"\n").is_err());
        assert!(Manifest::parse(&TERRAFORM.replace("command =", "comando =")).is_err());
    }

    #[test]
    fn a_manifest_of_another_api_is_explained_instead_of_failing_on_its_new_fields() {
        let future = TERRAFORM.replace("api = 1", "api = 2") + "\n[nuevo]\nx = 1\n";
        assert!(Manifest::parse(&future).is_err());
        assert_eq!(Manifest::declared_api(&future), Some(2));
        assert_eq!(Manifest::declared_api(TERRAFORM), Some(1));
        assert_eq!(Manifest::declared_api("no es toml ="), None);

        let m = with("api = 1", "api = 2");
        assert!(
            errors(&m)[0].contains("api = 2 no está soportada"),
            "{:?}",
            errors(&m)
        );
    }

    #[test]
    fn names_and_versions_are_checked() {
        for bad in ["", "Terraform", "con espacio", "a_b", "-x", "x-"] {
            let m = with("name = \"terraform\"", &format!("name = \"{bad}\""));
            assert!(
                errors(&m).iter().any(|e| e.starts_with("name:")),
                "'{bad}' {:?}",
                errors(&m)
            );
        }
        for bad in ["1", "1.0", "v1.0.0", "1.0.0-rc1", "a.b.c", "1..0"] {
            let m = with("version = \"0.1.0\"", &format!("version = \"{bad}\""));
            assert!(
                errors(&m).iter().any(|e| e.starts_with("version:")),
                "'{bad}' {:?}",
                errors(&m)
            );
        }
    }

    #[test]
    fn a_plugin_cannot_take_the_name_of_a_builtin_type() {
        for name in ["compose", "script", "sql", "gate", "backup"] {
            let m = with("name = \"terraform\"", &format!("name = \"{name}\""));
            assert!(
                errors(&m)
                    .iter()
                    .any(|e| e.contains("ya es un tipo de baton")),
                "{name}: {:?}",
                errors(&m)
            );
        }
    }

    #[test]
    fn requires_are_program_names_not_paths_or_options() {
        for bad in ["/usr/bin/terraform", "a b", "-rf", "", "a;b", "$(x)"] {
            let m = with(
                "requires = [\"terraform\"]",
                &format!("requires = [\"{bad}\"]"),
            );
            assert!(
                errors(&m).iter().any(|e| e.starts_with("requires[0]:")),
                "'{bad}' {:?}",
                errors(&m)
            );
        }
        let m = with(
            "requires = [\"terraform\"]",
            "requires = [\"az\", \"docker-compose\", \"g++\"]",
        );
        assert!(errors(&m).is_empty());
    }

    #[test]
    fn the_command_cannot_be_empty_and_unknown_placeholders_only_warn() {
        let m = with(
            "command = \"terraform init -input=false && terraform apply -input=false -auto-approve\"",
            "command = \"  \"",
        );
        assert!(errors(&m).iter().any(|e| e.starts_with("type.command:")));

        let m = with(
            "terraform plan -input=false\"",
            "terraform plan -var-file={ambiente}.tfvars -var x={nope}\"",
        );
        let issues = validate_manifest(&m);
        assert!(!has_errors(&issues), "{issues:?}");
        let warnings: Vec<_> = issues.iter().map(|i| i.message.as_str()).collect();
        assert!(
            warnings.iter().any(|w| w.contains("{nope}")),
            "{warnings:?}"
        );
        assert!(
            !warnings
                .iter()
                .any(|w| w.contains("desconocido {ambiente}")),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_dry_run_that_modifies_something_is_refused() {
        for bad in [
            "terraform apply",
            "terraform apply -auto-approve",
            "terraform plan && terraform destroy",
            "az deployment group create -g x",
            "docker compose up -d",
            "echo hola; rm -rf x",
            "(terraform apply)",
            "\"apply\"",
        ] {
            let m = with(
                "dry_run = \"terraform init -input=false && terraform plan -input=false\"",
                &format!("dry_run = {bad:?}"),
            );
            assert!(
                errors(&m).iter().any(|e| e.starts_with("type.dry_run:")),
                "'{bad}' {:?}",
                errors(&m)
            );
        }
    }

    #[test]
    fn read_only_dry_runs_are_accepted() {
        for ok in [
            "terraform init -input=false && terraform plan -input=false",
            "terraform plan -destroy",
            "az deployment group what-if -g x -f main.bicep",
            "bicep build main.bicep",
            "npm ci --dry-run",
            "docker compose config",
        ] {
            let m = with(
                "dry_run = \"terraform init -input=false && terraform plan -input=false\"",
                &format!("dry_run = {ok:?}"),
            );
            assert!(errors(&m).is_empty(), "'{ok}' {:?}", errors(&m));
        }
    }

    #[test]
    fn a_dry_run_equal_to_the_command_is_refused() {
        let m = manifest(
            r#"
api = 1
name = "x"
version = "0.1.0"
[type]
command = "make"
dry_run = "make"
"#,
        );
        assert!(
            errors(&m).iter().any(|e| e.contains("igual a command")),
            "{:?}",
            errors(&m)
        );
    }

    #[test]
    fn detect_patterns_stay_inside_the_project_and_must_be_valid_globs() {
        for bad in ["/etc/*", "../x/*.tf", ".baton/*", "~/x", "a[", ""] {
            let m = with("detect = [\"**/main.tf\"]", &format!("detect = [{bad:?}]"));
            assert!(
                errors(&m).iter().any(|e| e.starts_with("type.detect[0]:")),
                "'{bad}' {:?}",
                errors(&m)
            );
        }
        let many = (0..=MAX_DETECT)
            .map(|i| format!("\"a{i}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let m = with("detect = [\"**/main.tf\"]", &format!("detect = [{many}]"));
        assert!(errors(&m).iter().any(|e| e.contains("demasiados patrones")));
    }

    #[test]
    fn detect_without_scanned_only_warns() {
        let m = with("scanned = true", "scanned = false");
        let issues = validate_manifest(&m);
        assert!(!has_errors(&issues));
        assert!(
            issues
                .iter()
                .any(|i| i.message.contains("solo se usa con scanned"))
        );
    }

    #[test]
    fn the_spec_carries_everything_the_manifest_says() {
        let m = manifest(&TERRAFORM.replace("\"terraform\"\nversion", "\"t-spec\"\nversion"));
        let spec = m.to_spec();
        assert_eq!(spec.name, "t-spec");
        assert!(spec.scanned);
        assert!(!spec.has_services);
        assert!(spec.runs_command);
        assert!(!spec.own_interpreter);
        assert_eq!(spec.requires, Requires::Source);
        assert!(spec.default_command.unwrap().contains("terraform apply"));
        assert!(spec.dry_run.unwrap().contains("terraform plan"));
        assert_eq!(spec.detect, ["**/main.tf"]);
        assert_eq!(spec.binaries, ["terraform"]);
    }

    #[test]
    fn a_type_that_is_not_scanned_asks_for_a_command_not_a_source() {
        let m = manifest(
            r#"
api = 1
name = "t-noscan"
version = "0.1.0"
[type]
command = "make deploy"
"#,
        );
        assert_eq!(m.to_spec().requires, Requires::Command);
    }

    #[test]
    fn a_registered_plugin_works_in_a_plan_with_and_without_its_own_command() {
        let m = manifest(&TERRAFORM.replace("\"terraform\"\nversion", "\"t-in-plan\"\nversion"));
        let kind = m.register().unwrap();
        assert_eq!(kind.label(), "t-in-plan");

        let plan = |extra: &str| {
            Plan::parse(&format!(
                "name = \"p\"\n[[steps]]\nid = \"infra\"\nname = \"Infra\"\ntype = \"t-in-plan\"\n{extra}"
            ))
            .unwrap()
        };
        // sin origen no pasa: es un tipo escaneado
        let issues = crate::validate_plan(&plan(""), None);
        assert!(issues.iter().any(|i| i.message.contains("necesita source")));
        // con origen sí, y el comando sale del tipo
        let p = plan("source = \"infra/*/main.tf\"\n");
        assert!(!has_errors(&crate::validate_plan(&p, None)));
        assert!(
            p.steps[0]
                .command_template()
                .unwrap()
                .contains("terraform apply")
        );
        // un comando propio manda
        let p = plan("source = \"infra/*/main.tf\"\ncommand = \"make\"\n");
        assert_eq!(p.steps[0].command_template(), Some("make"));
    }

    #[test]
    fn registering_the_same_manifest_twice_is_fine_and_a_clashing_one_is_not() {
        let text = TERRAFORM.replace("\"terraform\"\nversion", "\"t-twice-m\"\nversion");
        let a = manifest(&text).register().unwrap();
        let b = manifest(&text).register().unwrap();
        assert_eq!(a, b);
        let other = manifest(&text.replace("terraform apply", "terraform apply -lock=false"));
        assert!(other.register().is_err());
    }

    #[test]
    fn a_non_scanned_plugin_needs_no_explicit_command_in_the_step() {
        let m = manifest(
            r#"
api = 1
name = "t-nocmd"
version = "0.1.0"
[type]
command = "make deploy"
"#,
        );
        m.register().unwrap();
        let p =
            Plan::parse("name = \"p\"\n[[steps]]\nid = \"a\"\nname = \"A\"\ntype = \"t-nocmd\"\n")
                .unwrap();
        assert!(!has_errors(&crate::validate_plan(&p, None)));
    }

    // ------------------------------------------------ lo destructivo

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn ansi_colours_are_removed_even_in_the_middle_of_a_phrase() {
        assert_eq!(
            strip_ansi("\u{1b}[1m# a.b\u{1b}[0m will be \u{1b}[31mdestroyed\u{1b}[0m"),
            "# a.b will be destroyed"
        );
        assert_eq!(strip_ansi("sin colores"), "sin colores");
        assert_eq!(strip_ansi("\u{1b}[38;5;196mrojo\u{1b}[0m"), "rojo");
        // una secuencia cortada al final no se traga el resto ni rompe
        assert_eq!(strip_ansi("texto\u{1b}["), "texto");
        assert_eq!(strip_ansi("ñandú\u{1b}[0m ✓"), "ñandú ✓");
    }

    #[test]
    fn a_coloured_phrase_split_by_escape_codes_is_still_found() {
        let out = lines(&[
            "Terraform will perform the following actions:",
            "\u{1b}[1m  # aws_instance.web\u{1b}[0m will be \u{1b}[1m\u{1b}[31mdestroyed\u{1b}[0m",
            "Plan: 0 to add, 0 to change, 1 to destroy.",
        ]);
        let hits = destructive_hits(&out, &["will be destroyed", "must be replaced"]);
        assert_eq!(hits, ["# aws_instance.web will be destroyed"]);
    }

    #[test]
    fn matching_ignores_case_and_surrounding_spaces() {
        let out = lines(&["   # x MUST BE REPLACED   "]);
        assert_eq!(
            destructive_hits(&out, &["  Must Be Replaced "]),
            ["# x MUST BE REPLACED"]
        );
    }

    #[test]
    fn a_plan_that_destroys_nothing_has_no_hits_even_if_it_says_to_destroy() {
        // la frase de resumen de terraform dice "0 to destroy": no es una coincidencia
        let out = lines(&[
            "Plan: 2 to add, 1 to change, 0 to destroy.",
            "# x will be created",
        ]);
        assert!(destructive_hits(&out, &["will be destroyed", "must be replaced"]).is_empty());
    }

    #[test]
    fn empty_patterns_and_empty_output_match_nothing() {
        assert!(destructive_hits(&lines(&["algo"]), &[]).is_empty());
        assert!(destructive_hits(&lines(&["algo"]), &["  ", ""]).is_empty());
        assert!(destructive_hits(&[], &["destroyed"]).is_empty());
    }

    #[test]
    fn destructive_phrases_need_a_dry_run_to_look_at() {
        let m = with(
            "dry_run = \"terraform init -input=false && terraform plan -input=false\"",
            "destructive = [\"will be destroyed\"]",
        );
        assert!(
            errors(&m).iter().any(|e| e.contains("declara un dry_run")),
            "{:?}",
            errors(&m)
        );
    }

    #[test]
    fn destructive_phrases_are_checked() {
        let base = "dry_run = \"terraform init -input=false && terraform plan -input=false\"";
        for ok in [
            "[\"will be destroyed\"]",
            "[\"will be destroyed\", \"must be replaced\"]",
            "[]",
        ] {
            let m = with(base, &format!("{base}\ndestructive = {ok}"));
            assert!(errors(&m).is_empty(), "{ok}: {:?}", errors(&m));
        }
        for bad in ["[\"x\"]", "[\"abc\"]", "[\"   \"]", "[\"\"]"] {
            let m = with(base, &format!("{base}\ndestructive = {bad}"));
            assert!(
                errors(&m)
                    .iter()
                    .any(|e| e.starts_with("type.destructive[0]:")),
                "{bad}: {:?}",
                errors(&m)
            );
        }
        let many = (0..=MAX_DESTRUCTIVE)
            .map(|i| format!("\"frase numero {i}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let m = with(base, &format!("{base}\ndestructive = [{many}]"));
        assert!(errors(&m).iter().any(|e| e.contains("demasiadas frases")));
    }

    #[test]
    fn the_spec_carries_the_destructive_phrases() {
        let base = "dry_run = \"terraform init -input=false && terraform plan -input=false\"";
        let text = TERRAFORM
            .replace("\"terraform\"\nversion", "\"t-destr-spec\"\nversion")
            .replace(
                base,
                &format!("{base}\ndestructive = [\"will be destroyed\", \"must be replaced\"]"),
            );
        let spec = manifest(&text).to_spec();
        assert_eq!(spec.destructive, ["will be destroyed", "must be replaced"]);
        let kind = manifest(&text).register().unwrap();
        assert_eq!(kind.destructive_patterns(), spec.destructive);
        // un tipo sin frases no pide revisar nada
        assert!(StepKind::Compose.destructive_patterns().is_empty());
    }

    // ------------------------------------------------ credenciales del plugin

    const AWS: &str = r#"
[[credentials]]
kind = "t-pc-aws"
fields = [
  { key = "ACCESS_KEY_ID" },
  { key = "SECRET_ACCESS_KEY", secret = true },
  { key = "REGION", optional = true, label = "región" },
]
[credentials.env]
AWS_ACCESS_KEY_ID = "{ACCESS_KEY_ID}"
AWS_SECRET_ACCESS_KEY = "{SECRET_ACCESS_KEY}"
AWS_DEFAULT_REGION = "{REGION}"
"#;

    fn with_credentials(name: &str, creds: &str) -> Manifest {
        manifest(&format!(
            "{}\n{creds}",
            TERRAFORM.replace("\"terraform\"\nversion", &format!("\"{name}\"\nversion"))
        ))
    }

    fn cred_errors(creds: &str) -> Vec<String> {
        errors(&with_credentials("t-pc-check", creds))
    }

    #[test]
    fn a_manifest_can_define_a_new_credential_kind_and_map_it_to_the_tool_variables() {
        let m = with_credentials("t-pc-ok", AWS);
        assert!(errors(&m).is_empty(), "{:?}", errors(&m));
        assert_eq!(m.credentials[0].kind, "t-pc-aws");
        assert_eq!(m.credentials[0].fields.len(), 3);
        assert_eq!(m.credentials[0].env["AWS_ACCESS_KEY_ID"], "{ACCESS_KEY_ID}");
    }

    #[test]
    fn a_manifest_can_use_a_baton_credential_kind_with_only_the_env() {
        let creds = r#"
[[credentials]]
kind = "docker"
[credentials.env]
REGISTRY_TOKEN = "{TOKEN}"
REGISTRY_URL = "https://{REGISTRY}/v2"
"#;
        assert!(cred_errors(creds).is_empty(), "{:?}", cred_errors(creds));
    }

    #[test]
    fn registering_defines_the_kind_with_its_fields_labels_and_secrecy() {
        let m = with_credentials("t-pc-reg", &AWS.replace("t-pc-aws", "t-pc-reg-aws"));
        let step = m.register().unwrap();
        let kind = CredentialKind::from_name("t-pc-reg-aws").unwrap();
        assert!(!kind.is_builtin());
        let fields = crate::credential::fields_for(kind);
        let keys: Vec<(&str, &str, bool, bool)> = fields
            .iter()
            .map(|f| (f.key, f.label, f.secret, f.optional))
            .collect();
        assert_eq!(
            keys,
            [
                ("ACCESS_KEY_ID", "access key id", false, false),
                ("SECRET_ACCESS_KEY", "secret access key", true, false),
                ("REGION", "región", false, true),
            ]
        );
        // y el tipo de paso sabe qué usa y con qué nombres
        let uses = step.credential_uses();
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].kind, "t-pc-reg-aws");
        assert!(
            uses[0]
                .env
                .contains(&("AWS_SECRET_ACCESS_KEY", "{SECRET_ACCESS_KEY}"))
        );
    }

    #[test]
    fn two_plugins_may_share_a_kind_if_they_define_it_the_same_but_not_differently() {
        let creds = AWS.replace("t-pc-aws", "t-pc-shared");
        with_credentials("t-pc-share-a", &creds).register().unwrap();
        with_credentials("t-pc-share-b", &creds).register().unwrap();
        // el mismo tipo con otros campos: el segundo no se carga
        let different = creds.replace(
            "  { key = \"REGION\", optional = true, label = \"región\" },\n",
            "",
        );
        let different = different.replace("AWS_DEFAULT_REGION = \"{REGION}\"\n", "");
        let e = with_credentials("t-pc-share-c", &different)
            .register()
            .unwrap_err();
        assert!(e.contains("otro plugin con campos distintos"), "{e}");
    }

    #[test]
    fn a_plugin_cannot_redefine_a_baton_credential_kind() {
        let creds = r#"
[[credentials]]
kind = "docker"
fields = [{ key = "TOKEN", secret = true }]
[credentials.env]
X = "{TOKEN}"
"#;
        let e = cred_errors(creds);
        assert!(
            e.iter()
                .any(|m| m.contains("es un tipo de baton y ya tiene sus campos")),
            "{e:?}"
        );
    }

    #[test]
    fn a_new_kind_needs_its_fields_and_a_credential_needs_an_env() {
        let no_fields = "[[credentials]]\nkind = \"t-pc-nf\"\n[credentials.env]\nX = \"{A}\"\n";
        assert!(
            cred_errors(no_fields)
                .iter()
                .any(|m| m.contains("declara sus campos")),
            "{:?}",
            cred_errors(no_fields)
        );
        let no_env = "[[credentials]]\nkind = \"t-pc-ne\"\nfields = [{ key = \"A\" }]\n";
        assert!(
            cred_errors(no_env).iter().any(|m| m.contains("sin env")),
            "{:?}",
            cred_errors(no_env)
        );
    }

    #[test]
    fn the_env_templates_are_checked_against_the_declared_fields() {
        let mk = |tpl: &str| {
            format!(
                "[[credentials]]\nkind = \"t-pc-tpl\"\nfields = [{{ key = \"A\" }}]\n[credentials.env]\nX = \"{tpl}\"\n"
            )
        };
        for ok in ["{A}", "id-{A}", "{A}:{A}", "https://{A}/x"] {
            assert!(
                cred_errors(&mk(ok)).is_empty(),
                "{ok}: {:?}",
                cred_errors(&mk(ok))
            );
        }
        for (bad, why) in [
            ("{B}", "no es un campo de 't-pc-tpl'"),
            ("literal", "no usa ningún {CAMPO}"),
            ("{A", "sin cerrar"),
            ("{a}", "no es un campo"),
            ("{}", "no es un campo"),
            ("{A B}", "no es un campo"),
            ("  ", "no puede estar vacía"),
        ] {
            let e = cred_errors(&mk(bad));
            assert!(e.iter().any(|m| m.contains(why)), "'{bad}' ({why}): {e:?}");
        }
    }

    #[test]
    fn dangerous_variables_cannot_be_defined_by_a_credential() {
        for name in [
            "PATH",
            "HOME",
            "SHELL",
            "IFS",
            "BASH_ENV",
            "ENV",
            "PS4",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "DYLD_INSERT_LIBRARIES",
            "BATON_ASKPASS_KEYS",
            "BATON_X",
        ] {
            let creds = format!(
                "[[credentials]]\nkind = \"t-pc-env\"\nfields = [{{ key = \"A\" }}]\n[credentials.env]\n{name} = \"{{A}}\"\n"
            );
            let e = cred_errors(&creds);
            assert!(
                e.iter()
                    .any(|m| m.contains(&format!("no puede definir {name}"))),
                "{name}: {e:?}"
            );
        }
        for bad in ["lower", "A-B", "1A", "A B"] {
            let creds = format!(
                "[[credentials]]\nkind = \"t-pc-env2\"\nfields = [{{ key = \"A\" }}]\n[credentials.env]\n\"{bad}\" = \"{{A}}\"\n"
            );
            assert!(
                cred_errors(&creds)
                    .iter()
                    .any(|m| m.contains("no es un nombre de variable")),
                "{bad}: {:?}",
                cred_errors(&creds)
            );
        }
    }

    #[test]
    fn field_declarations_are_checked() {
        let mk = |fields: &str| {
            format!(
                "[[credentials]]\nkind = \"t-pc-f\"\nfields = {fields}\n[credentials.env]\nX = \"{{A}}\"\n"
            )
        };
        assert!(
            cred_errors(&mk("[{ key = \"a\" }]"))
                .iter()
                .any(|m| m.contains("clave 'a' no válida"))
        );
        assert!(
            cred_errors(&mk("[{ key = \"A\" }, { key = \"A\" }]"))
                .iter()
                .any(|m| m.contains("repetido"))
        );
        assert!(
            cred_errors(&mk("[{ key = \"A\", label = \"  \" }]"))
                .iter()
                .any(|m| m.contains("etiqueta"))
        );
        assert!(
            cred_errors(&mk("[]"))
                .iter()
                .any(|m| m.contains("declara sus campos"))
        );
        let many = (0..=MAX_FIELDS)
            .map(|i| format!("{{ key = \"K{i}\" }}"))
            .collect::<Vec<_>>()
            .join(", ");
        assert!(
            cred_errors(&mk(&format!("[{many}]")))
                .iter()
                .any(|m| m.contains("demasiados campos"))
        );
        // un campo desconocido en la declaración es un error de lectura (un typo no se ignora)
        let typo = format!(
            "{TERRAFORM}\n[[credentials]]\nkind = \"t-pc-typo\"\nfields = [{{ key = \"A\", secreto = true }}]\n"
        );
        assert!(Manifest::parse(&typo).is_err());
    }

    #[test]
    fn a_kind_declared_twice_or_too_many_credentials_are_refused() {
        let one = "[[credentials]]\nkind = \"t-pc-dup\"\nfields = [{ key = \"A\" }]\n[credentials.env]\nX = \"{A}\"\n";
        let e = cred_errors(&format!("{one}\n{one}"));
        assert!(e.iter().any(|m| m.contains("declarada dos veces")), "{e:?}");
        let many: String = (0..=MAX_CREDENTIALS)
            .map(|i| format!("[[credentials]]\nkind = \"t-pc-m{i}\"\nfields = [{{ key = \"A\" }}]\n[credentials.env]\nX = \"{{A}}\"\n"))
            .collect();
        assert!(
            cred_errors(&many)
                .iter()
                .any(|m| m.contains("demasiadas credenciales"))
        );
    }

    #[test]
    fn rendering_replaces_fields_in_one_pass_and_drops_the_variable_when_a_field_is_missing() {
        let lookup = |k: &str| match k {
            "A" => Some("uno".to_string()),
            "B" => Some("{A}".to_string()), // un valor que parece otro campo
            "EMPTY" => Some(String::new()),
            _ => None,
        };
        assert_eq!(render_env("{A}", lookup).as_deref(), Some("uno"));
        assert_eq!(
            render_env("x-{A}-{A}", lookup).as_deref(),
            Some("x-uno-uno")
        );
        assert_eq!(
            render_env("https://{A}/v2", lookup).as_deref(),
            Some("https://uno/v2")
        );
        // el valor de B es el texto "{A}": no se vuelve a expandir
        assert_eq!(render_env("{B}", lookup).as_deref(), Some("{A}"));
        assert_eq!(render_env("{A}{B}", lookup).as_deref(), Some("uno{A}"));
        // faltante o vacío: no se define
        assert_eq!(render_env("{NADA}", lookup), None);
        assert_eq!(render_env("{A}-{EMPTY}", lookup), None);
        assert_eq!(render_env("{A", lookup), None);
        assert_eq!(
            render_env("sin campos", lookup).as_deref(),
            Some("sin campos")
        );
    }

    #[test]
    fn placeholders_are_listed_and_odd_ones_explained() {
        assert_eq!(env_placeholders("{A}-{B_2}").unwrap(), ["A", "B_2"]);
        assert!(env_placeholders("sin").unwrap().is_empty());
        assert!(
            env_placeholders("{a}")
                .unwrap_err()
                .contains("no es un campo")
        );
        assert!(env_placeholders("{A").unwrap_err().contains("sin cerrar"));
    }
}
