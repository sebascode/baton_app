<!-- Los PR van contra `develop`, nunca contra `main`. -->

## Qué cambia y por qué

## Privacidad y seguridad

Marca lo que corresponda. Si algo queda sin marcar, explica por qué.

- [ ] No agrega ninguna conexión de red nueva. Si la agrega, está descrita abajo (a dónde, qué envía, quién la activa).
- [ ] No envía credenciales, llaves, contenido de `.baton/` ni datos de servidores a ningún lugar.
- [ ] Los secretos no viajan en argumentos de procesos ni quedan en logs o mensajes de error (pasan por `mask::redact`).
- [ ] No agrega dependencias nuevas, o las agrega y explico para qué sirven.
- [ ] No toca `.github/`, `scripts/`, `build.rs` ni el empaquetado, o explico qué cambia.

## Origen del código

- [ ] Es código mío, o de una fuente con licencia compatible con MIT que indico abajo.
- [ ] Mis commits llevan `Signed-off-by` (`git commit -s`), según el [DCO](https://developercertificate.org/).

## Cómo lo probé
