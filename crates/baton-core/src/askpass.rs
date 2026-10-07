//! Frases secretas de llaves ssh sin terminal: `ssh` las pide a un programa externo
//! (`SSH_ASKPASS`) cuando no puede preguntar, y ese programa es el propio `baton`, que las lee de
//! una variable de entorno que solo existe en el proceso de `ssh`. Este módulo es la parte pura:
//! cómo se codifican las frases en esa variable y cómo se responde a lo que `ssh` pregunta.

/// Separa una llave de la siguiente.
const RECORD: char = '\u{1e}';
/// Separa la ruta de una llave de su frase secreta.
const UNIT: char = '\u{1f}';

/// Nombre de la variable con las frases (solo la define el runner, en el entorno de `ssh`).
pub const KEYS_VAR: &str = "BATON_ASKPASS_KEYS";

/// `(ruta de la llave, frase secreta)` codificado para la variable de entorno.
pub fn encode(keys: &[(String, String)]) -> String {
    keys.iter()
        .map(|(path, phrase)| format!("{path}{UNIT}{phrase}"))
        .collect::<Vec<_>>()
        .join(&RECORD.to_string())
}

fn decode(text: &str) -> Vec<(&str, &str)> {
    text.split(RECORD)
        .filter_map(|r| r.split_once(UNIT))
        .collect()
}

/// Qué responder a lo que pregunta `ssh`, o `None` para negarse. Solo se contesta a una frase
/// secreta (`Enter passphrase for key '/ruta': `): una contraseña, una confirmación de huella
/// (`yes/no`) o cualquier otra cosa no se responde nunca, así `ssh` falla en vez de aceptar algo
/// a ciegas.
///
/// La frase es la de la llave cuya ruta aparece en la pregunta; si ninguna aparece (`ssh` puede
/// mostrar la ruta expandida de otra forma) y solo hay una llave, esa.
pub fn answer(keys: &str, prompt: &str) -> Option<String> {
    if !prompt.to_ascii_lowercase().contains("passphrase") {
        return None;
    }
    let keys = decode(keys);
    keys.iter()
        .find(|(path, _)| !path.is_empty() && prompt.contains(path))
        .or_else(|| (keys.len() == 1).then(|| &keys[0]))
        .map(|(_, phrase)| (*phrase).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> String {
        encode(&[
            ("/home/x/.ssh/destino".into(), "frase uno".into()),
            ("/home/x/.ssh/bastion".into(), "frase: dos".into()),
        ])
    }

    #[test]
    fn answers_with_the_phrase_of_the_key_named_in_the_question() {
        let k = keys();
        assert_eq!(
            answer(&k, "Enter passphrase for key '/home/x/.ssh/bastion': "),
            Some("frase: dos".into())
        );
        assert_eq!(
            answer(&k, "Enter passphrase for \"/home/x/.ssh/destino\": "),
            Some("frase uno".into())
        );
    }

    #[test]
    fn with_several_keys_an_unknown_one_is_refused_but_a_single_key_is_used() {
        assert_eq!(answer(&keys(), "Enter passphrase for key '/otra': "), None);
        let one = encode(&[("/k".into(), "solo".into())]);
        assert_eq!(
            answer(&one, "Enter passphrase for key '/ruta/expandida': "),
            Some("solo".into())
        );
    }

    #[test]
    fn it_never_answers_anything_but_a_passphrase() {
        let k = keys();
        for q in [
            "user@host's password: ",
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
            "Verification code: ",
            "",
        ] {
            assert_eq!(answer(&k, q), None, "{q:?}");
        }
    }

    #[test]
    fn phrases_may_contain_spaces_tabs_newlines_and_quotes() {
        let k = encode(&[("/k".into(), "a b\tc\nd'e\"f".into())]);
        assert_eq!(
            answer(&k, "Enter passphrase for key '/k': "),
            Some("a b\tc\nd'e\"f".into())
        );
    }

    #[test]
    fn nothing_encoded_answers_nothing() {
        assert_eq!(answer("", "Enter passphrase for key '/k': "), None);
        assert_eq!(encode(&[]), "");
    }
}
