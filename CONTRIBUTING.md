# Cómo contribuir

Gracias por querer aportar. Este proyecto maneja credenciales y llaves de otras personas, así que
la revisión es estricta a propósito. Lee esto antes de abrir un PR.

## Ramas (git flow)

| Rama | Para qué |
|---|---|
| `main` | Solo versiones publicadas. Cada commit es un release con su tag `vX.Y.Z`. No se trabaja aquí. |
| `develop` | Integración. Es la rama por defecto y **el destino de todos los PR**. |
| `feature/*` | Una funcionalidad o corrección. Sale de `develop` y vuelve a `develop`. |
| `release/X.Y.Z` | Preparación de una versión. Sale de `develop`, va a `main` con el tag y se fusiona de vuelta en `develop`. |
| `hotfix/*` | Arreglo urgente sobre lo publicado. Sale de `main`, va a `main` y a `develop`. |

Si haces un fork, trabaja en `feature/<algo>` desde `develop` y abre el PR contra `develop`. Un PR
contra `main` se cerrará.

## Antes de abrir el PR

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Si cambias comandos, opciones o códigos de salida, actualiza `man/baton.1` (hay una prueba que lo
exige).

## Firma de origen (DCO)

Cada commit debe llevar `Signed-off-by`, que es tu declaración de que tienes derecho a aportar ese
código bajo la licencia MIT del proyecto ([texto del DCO](https://developercertificate.org/)):

```bash
git commit -s -m "mensaje"
git rebase --signoff develop      # para firmar commits que ya hiciste
```

No copies código de otros proyectos salvo que su licencia sea compatible con MIT, y en ese caso
indícalo en el PR. Un chequeo automático rechaza commits sin firma.

## Reglas de privacidad

Un PR no se acepta si hace alguna de estas cosas:

- Envía credenciales, llaves, el contenido de `.baton/` o datos de servidores a cualquier lugar.
- Agrega una conexión de red que la persona no haya pedido o configurado. Baton no tiene
  telemetría ni la tendrá.
- Pone un secreto en un argumento de proceso, en un log o en un mensaje de error.
- Agrega una dependencia de red, TLS, telemetría o reporte de errores. `deny.toml` lo bloquea y
  cambiarlo exige una justificación aparte.
- Modifica `.github/`, `scripts/`, `build.rs` o el empaquetado sin que sea el objetivo declarado
  del PR.

Las rutas más delicadas están en `.github/CODEOWNERS`: ahí toda revisión es obligatoria. Lo que
sale de la máquina está listado en [SECURITY.md](SECURITY.md).

## Plugins

Los plugins viven en su propio repositorio, no en este. Si quieres escribir uno (para Terraform,
Bicep, Node o lo que uses), la guía está en [docs/plugins.md](docs/plugins.md). Cambios al
**sistema** de plugins (el formato del manifiesto, la instalación, las credenciales) sí son para este
repositorio y se revisan con el mismo rigor que el resto de las rutas sensibles.

## Estilo

Textos de interfaz en español, en sentence case y sin signos de exclamación. En textos y
comentarios no se usan guiones largos: usa `-` o paréntesis. Los mensajes de commit describen el
cambio de forma directa, sin prefijos como `feat:`.
