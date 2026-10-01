//! Integración con el prompt de la terminal: `baton shell-init <shell>` imprime el código que
//! muestra `(baton:proyecto)` mientras se está dentro de un proyecto (como `(venv)` o la rama de
//! git), y `baton prompt` la misma etiqueta desde Rust.
//!
//! El código de cada shell **no llama a baton en cada prompt**: busca el proyecto hacia arriba
//! con comprobaciones de carpeta, igual que `Project::discover` (una carpeta con `baton/plans/` o
//! `.baton/`). Esa regla vive en dos sitios, así que hay pruebas que las comparan.

/// Formato por defecto de la etiqueta (`baton prompt --format`).
pub const DEFAULT_FORMAT: &str = "(baton:{name})";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    /// `bash`, `zsh`, `fish`, o una ruta como `/bin/bash` (lo que trae `$SHELL`).
    pub fn parse(text: &str) -> Option<Shell> {
        match text.trim().rsplit('/').next()? {
            "bash" => Some(Shell::Bash),
            "zsh" => Some(Shell::Zsh),
            "fish" => Some(Shell::Fish),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        }
    }

    /// El archivo donde se agrega la línea `eval`.
    pub fn rc_file(self) -> &'static str {
        match self {
            Shell::Bash => "~/.bashrc",
            Shell::Zsh => "~/.zshrc",
            Shell::Fish => "~/.config/fish/config.fish",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitOptions {
    /// Antepone la etiqueta al prompt automáticamente. Sin esto solo se define `__baton_ps1`,
    /// para quien arma su `PS1` a mano (`PS1='$(__baton_ps1)\u@\h \w\$ '`).
    pub prefix: bool,
    /// La etiqueta en cian.
    pub color: bool,
}

impl Default for InitOptions {
    fn default() -> Self {
        InitOptions {
            prefix: true,
            color: false,
        }
    }
}

/// El nombre que se muestra de un proyecto: su carpeta (`/` si la raíz es `/`).
pub fn project_label(root: &std::path::Path) -> String {
    root.file_name()
        .map_or_else(|| "/".to_string(), |n| n.to_string_lossy().into_owned())
}

/// `{name}` (la carpeta del proyecto) y `{root}` (su ruta completa).
pub fn format_prompt(format: &str, name: &str, root: &str) -> String {
    format.replace("{name}", name).replace("{root}", root)
}

/// Busca el proyecto subiendo desde `$PWD`: deja la raíz en `__BATON_ROOT` (vacía si no hay).
/// Sin procesos: solo `[ -d ]`.
const POSIX_FIND: &str = r#"__baton_find() {
  __BATON_ROOT=""
  local d="$PWD"
  while :; do
    if [ -d "$d/baton/plans" ] || [ -d "$d/.baton" ]; then
      __BATON_ROOT="$d"
      return 0
    fi
    [ "$d" = "/" ] && return 1
    d="${d%/*}"
    [ -z "$d" ] && d="/"
  done
}
"#;

pub fn init_script(shell: Shell, opts: InitOptions) -> String {
    let mut out = format!(
        "# baton: muestra el proyecto en el prompt, por ejemplo (baton:app1).\n\
         # Para activarlo, agrega esta línea a {rc}:\n\
         #   {line}\n",
        rc = shell.rc_file(),
        line = match shell {
            Shell::Fish => "baton shell-init fish | source".to_string(),
            s => format!("eval \"$(baton shell-init {})\"", s.name()),
        }
    );
    out.push_str(&match shell {
        Shell::Bash => bash(opts),
        Shell::Zsh => zsh(opts),
        Shell::Fish => fish(opts),
    });
    out
}

/// `__baton_ps1` para bash y zsh: imprime la etiqueta con un espacio al final (o nada). Pensada
/// para `$(...)` dentro de un `PS1` armado a mano, como `__git_ps1`.
fn posix_ps1(color: bool) -> String {
    let (on, off) = if color {
        ("\\033[36m", "\\033[0m")
    } else {
        ("", "")
    };
    format!(
        r#"__baton_ps1() {{
  __baton_find || return 0
  local name="${{__BATON_ROOT##*/}}"
  [ -z "$name" ] && name="/"
  printf '{on}(baton:%s){off} ' "$name"
}}
"#
    )
}

fn bash(opts: InitOptions) -> String {
    let mut s = String::from(POSIX_FIND);
    s.push_str(&posix_ps1(opts.color));
    if !opts.prefix {
        return s;
    }
    // Los `\[ \]` marcan lo que no ocupa ancho (los colores), para que bash calcule bien la línea.
    let (on, off) = if opts.color {
        (r"\[\e[36m\]", r"\[\e[0m\]")
    } else {
        ("", "")
    };
    s.push_str(&format!(
        r#"# Antepone la etiqueta al prompt y la quita al salir del proyecto. El nombre de la carpeta
# se sanea: va dentro de PS1, que bash vuelve a expandir ($(...), comillas invertidas), y una
# carpeta con ese nombre ejecutaría comandos al dibujar el prompt.
__baton_prompt_command() {{
  __baton_find
  local mark=""
  if [ -n "$__BATON_ROOT" ]; then
    local name="${{__BATON_ROOT##*/}}"
    [ -z "$name" ] && name="/"
    name="${{name//[^[:alnum:]._+@ -]/?}}"
    mark="{on}(baton:${{name}}){off} "
  fi
  PS1="${{PS1#"$__BATON_MARK"}}"
  __BATON_MARK="$mark"
  PS1="${{mark}}${{PS1}}"
}}
if [ -z "${{__BATON_SHELL_INIT:-}}" ]; then
  __BATON_SHELL_INIT=1
  if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    PROMPT_COMMAND=(__baton_prompt_command "${{PROMPT_COMMAND[@]}}")
  else
    PROMPT_COMMAND="__baton_prompt_command${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}"
  fi
fi
"#
    ));
    s
}

fn zsh(opts: InitOptions) -> String {
    let mut s = String::from(POSIX_FIND);
    s.push_str(&posix_ps1(opts.color));
    if !opts.prefix {
        return s;
    }
    let (on, off) = if opts.color {
        ("%F{cyan}", "%f")
    } else {
        ("", "")
    };
    s.push_str(&format!(
        r#"# Antepone la etiqueta al prompt y la quita al salir del proyecto (el nombre se sanea: el
# prompt de zsh interpreta `%` y, con prompt_subst, también `$`).
__baton_precmd() {{
  __baton_find
  local mark=""
  if [ -n "$__BATON_ROOT" ]; then
    local name="${{__BATON_ROOT##*/}}"
    [ -z "$name" ] && name="/"
    name="${{name//[^[:alnum:]._+@ -]/?}}"
    mark="{on}(baton:${{name}}){off} "
  fi
  PS1="${{PS1#"$__BATON_MARK"}}"
  __BATON_MARK="$mark"
  PS1="${{mark}}${{PS1}}"
}}
autoload -Uz add-zsh-hook
add-zsh-hook precmd __baton_precmd
"#
    ));
    s
}

fn fish(opts: InitOptions) -> String {
    let (on, off) = if opts.color {
        ("set_color cyan; ", "; set_color normal")
    } else {
        ("", "")
    };
    let mut s = String::from(
        r#"function __baton_find
  set -g __BATON_ROOT ""
  set -l d $PWD
  while true
    if test -d "$d/baton/plans"; or test -d "$d/.baton"
      set -g __BATON_ROOT $d
      return 0
    end
    test "$d" = "/"; and return 1
    set d (path dirname $d)
  end
end
function __baton_ps1
  __baton_find; or return 0
  set -l name (path basename $__BATON_ROOT)
  test -z "$name"; and set name "/"
  printf '(baton:%s) ' $name
end
"#,
    );
    if opts.prefix {
        s.push_str(&format!(
            r#"# fish imprime con printf, que no vuelve a interpretar el nombre.
if not functions -q __baton_original_fish_prompt
  functions -c fish_prompt __baton_original_fish_prompt
  function fish_prompt
    if __baton_find
      set -l name (path basename $__BATON_ROOT)
      test -z "$name"; and set name "/"
      {on}printf '(baton:%s)' $name{off}
      printf ' '
    end
    __baton_original_fish_prompt
  end
end
"#
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn shell_names_and_paths_are_understood() {
        assert_eq!(Shell::parse("bash"), Some(Shell::Bash));
        assert_eq!(Shell::parse("/bin/zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::parse("/usr/bin/fish\n"), Some(Shell::Fish));
        assert_eq!(Shell::parse("tcsh"), None);
        assert_eq!(Shell::parse(""), None);
    }

    #[test]
    fn the_label_is_the_project_folder() {
        assert_eq!(project_label(Path::new("/home/x/app1")), "app1");
        assert_eq!(project_label(Path::new("/")), "/");
        assert_eq!(
            format_prompt(DEFAULT_FORMAT, "app1", "/home/x/app1"),
            "(baton:app1)"
        );
        assert_eq!(
            format_prompt("[{name} en {root}]", "a", "/r/a"),
            "[a en /r/a]"
        );
    }

    #[test]
    fn every_script_explains_how_to_activate_it_and_defines_the_function() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let s = init_script(shell, InitOptions::default());
            assert!(s.starts_with("# baton:"), "{shell:?}");
            assert!(s.contains(shell.rc_file()), "{shell:?}");
            assert!(s.contains("__baton_ps1"), "{shell:?}");
            assert!(s.contains("__baton_find"), "{shell:?}");
        }
    }

    #[test]
    fn without_the_prefix_only_the_function_is_defined() {
        let opts = InitOptions {
            prefix: false,
            color: false,
        };
        let bash = init_script(Shell::Bash, opts);
        assert!(bash.contains("__baton_ps1") && !bash.contains("PROMPT_COMMAND="));
        let zsh = init_script(Shell::Zsh, opts);
        assert!(!zsh.contains("add-zsh-hook"));
        let fish = init_script(Shell::Fish, opts);
        assert!(!fish.contains("fish_prompt"));
    }

    #[test]
    fn color_only_changes_the_label_style() {
        let plain = init_script(Shell::Bash, InitOptions::default());
        let color = init_script(
            Shell::Bash,
            InitOptions {
                prefix: true,
                color: true,
            },
        );
        assert!(!plain.contains("36m") && color.contains("36m"));
        assert!(
            color.contains(r"\[\e[36m\]"),
            "los colores van entre \\[ \\]"
        );
    }

    #[test]
    fn the_bash_prefix_sanitizes_the_folder_name() {
        let s = init_script(Shell::Bash, InitOptions::default());
        assert!(s.contains("name=\"${name//[^[:alnum:]._+@ -]/?}\""), "{s}");
    }
}
