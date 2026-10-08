# baton

Herramienta de terminal para orquestar instalaciones y despliegues. Defines un plan con pasos (docker compose, Dockerfile, scripts sh, SQL), y baton los ejecuta en orden mostrando en todo momento qué está pasando, como un pipeline de CI pero en tu terminal.

Está pensada para ser simple y amigable: pantallas con teclado, backup y rollback siempre opcionales, y nada sale de tu máquina que no deba.

> Estado: proyecto personal en desarrollo (versión 0.4). Solo se ha probado en Linux y macOS con Apple Silicon.

## Qué es

Un plan es una lista ordenada de pasos. Cada paso apunta a archivos de tu proyecto y baton sabe cómo ejecutarlos:

| Tipo | Qué hace |
|---|---|
| `compose` | levanta un `docker-compose.yml` |
| `dockerfile` | construye una imagen |
| `script` | corre archivos `.sh` |
| `sql` | ejecuta archivos `.sql` contra PostgreSQL, MySQL o MariaDB, o un archivo SQLite |
| `comando`, `check`, `backup`, `gate` | un comando suelto, una verificación, un respaldo, o una pausa |

Los pasos pueden correr en tu máquina, por ssh (con bastion opcional) o en un docker context.

## Cómo lo hace

- **Plan versionado**: se guarda en `baton/plans/<plan>.toml`, junto a tu código.
- **Validación previa**: antes de ejecutar nada revisa todo y te lista los problemas de una vez.
- **Gates**: un paso puede esperar una confirmación manual o verificaciones automáticas (healthcheck de Docker, HTTP, comando, contenedor corriendo) antes de seguir.
- **Fallos**: si algo falla, puedes reintentar, ver el log completo, abrir una shell, hacer rollback o abortar, y luego reanudar desde donde quedó.
- **Credenciales**: viven en `.baton/` (se agrega al `.gitignore`, permisos 600), o en variables de entorno, o en un gestor como Vault o Azure Key Vault. Cada comando recibe solo las que necesita y se tachan de los logs. `.baton/` nunca sale de tu máquina.
- **Sin terminal** (CI): no pregunta nada; si falta algo, falla diciendo qué.

## Instalación

Elige una:

**Una línea** (Linux y macOS con Apple Silicon; sin Rust ni Homebrew). Baja el binario de tu plataforma, comprueba su suma sha256 y lo deja en `~/.local/bin`:

```sh
curl -fsSL https://raw.githubusercontent.com/sebascode/baton_app/main/scripts/get.sh | sh
```

Con otra versión o carpeta: `... | sh -s -- --version 0.3.0 --dir /usr/local/bin`.

**Homebrew** (macOS y Linux), compilando desde el código fuente:

```sh
brew install sebascode/baton/baton
```

**Paquetes de Linux**: cada [release](https://github.com/sebascode/baton_app/releases/latest) trae un `.deb` (Debian, Ubuntu...) y un `.rpm` (Fedora, RHEL...) para x86_64 y arm64, con su `.sha256`:

```sh
sudo apt install ./baton_0.3.0-1_amd64.deb      # o: sudo dnf install ./baton-0.3.0-1.x86_64.rpm
```

**A mano**, con Rust 1.88 o superior:

```sh
git clone https://github.com/sebascode/baton_app
cd baton_app
scripts/install.sh        # instala en ~/.local/bin y deja el manual (man baton)
```

Para volver a la versión anterior: `scripts/install.sh --rollback`. Comprueba con `baton version`.

### Actualizar

- Instalado con la línea de arriba, a mano o con un binario del release: `baton update` (abajo).
- Con Homebrew: `brew upgrade baton`.
- Con un `.deb` o `.rpm`: baja el paquete de la versión nueva e instálalo igual (`baton update` te lo recuerda y no toca `/usr/bin`).

```sh
baton update --check      # ¿hay una versión nueva? (una consulta a GitHub, no descarga nada)
baton update              # baja el binario del release, verifica su sha256 y lo reemplaza
baton update --rollback   # vuelve a la versión anterior
```

Cada release de GitHub trae binarios para Linux (x86_64 y aarch64) y macOS (Apple Silicon). `baton update` necesita `curl`, `tar` y `sha256sum` (o `shasum`), y nunca consulta la red por su cuenta.

## Cómo usarlo

Prueba las pantallas con datos falsos, sin tocar nada:

```sh
baton demo
```

En tu proyecto:

```sh
baton init             # escanea la carpeta (compose, Dockerfile, .sh, .sql) y arma un plan
baton start            # revisa y edita el plan en la TUI
baton run              # ejecuta el plan
```

Otros comandos útiles:

```sh
baton validate                # revisa el plan sin ejecutar
baton run --dry-run           # simula, no deja rastro
baton run --resume            # continúa un plan que falló a la mitad
baton last                    # cómo terminó la última ejecución y, si falló, el error
baton history                 # las ejecuciones anteriores del plan, una por línea
baton db -c "select * from clientes"   # consulta una base del plan (PostgreSQL, MySQL o SQLite), solo lectura
baton db local                # o abre una sesión interactiva con historial (\? ayuda)
baton rollback                # deshace la última ejecución
baton import pipeline.yml     # convierte GitHub Actions, GitLab CI o Azure Pipelines en un plan
baton config                  # destinos, logs y credenciales
```

Un plan mínimo:

```toml
name = "instalar"

[[steps]]
id = "servicios"
name = "Levantar servicios"
type = "compose"
source = "services/*/docker-compose.yml"
```

### En tus scripts bash

baton también ofrece preguntas para scripts. La interfaz va a la terminal y solo la respuesta a stdout:

```sh
env=$(baton select "¿Ambiente?" dev staging prod --default dev)
if baton confirm "¿Desplegar a $env?" --default no; then ...; fi
```

Sin terminal se resuelven con `--default` o con variables `BATON_<NOMBRE>`.

### Etiqueta en el prompt

```sh
eval "$(baton shell-init bash)"   # también zsh y fish; muestra (baton:proyecto) dentro de un proyecto
```

Más detalle con `baton --help` y `man baton`.

### Generar un plan con una IA

Si quieres que un asistente (Claude Code, Gemini, DeepSeek u otro) escriba el plan de tu proyecto, pásale [docs/ai-guide.md](docs/ai-guide.md). Explica el formato, las reglas y cómo comprobar el resultado con `baton validate` y `baton run --dry-run`.

### Plugins

Un plugin añade un tipo de paso nuevo (`terraform`, `make`, el que uses) con un archivo de texto que
dice qué comando ejecutar. No tiene código, así que sirve para cualquier herramienta, en el lenguaje
que esté escrita. baton conserva la ejecución, las credenciales, los logs y los gates.

```sh
baton plugin new mi-plugin        # crea un manifiesto para empezar
baton plugin validate mi-plugin   # lo revisa y muestra lo que ejecutaría
baton plugin add ./mi-plugin      # lo instala (o github:dueño/repo@v1.0.0, con commit firmado)
baton plugin list                 # los instalados
```

Para escribir uno, con un tutorial de cinco minutos, la referencia completa y cómo publicarlo
firmado en GitHub: [docs/plugins.md](docs/plugins.md).

## Línea de tiempo

Qué se fue agregando y cuándo (las fechas son las de los commits del repositorio).

| Fecha | Qué llegó |
|---|---|
| 2026-09-24 | Inicio. Modelo del plan y de la configuración en TOML, validación con `archivo:línea:columna` (`baton validate`) y todas las pantallas de la terminal con datos de mentira (`baton demo`): vista previa del plan, ejecución, fallo, resumen, editor de pasos, gates, pipeline. |
| 2026-09-27 | **Ejecución real.** Compose y Dockerfile con timeout, reintentos, rollback, backup de volúmenes, gates manuales, `--resume` y `--dry-run`; log, estado y modo texto para CI (`baton run`). Gates automáticos (healthcheck, HTTP, comando, contenedor corriendo) y un editor que guarda sin perder tus comentarios. |
| 2026-09-28 | `baton init`, que arma el plan escaneando la carpeta. Credenciales reales con enmascarado, "no volver a preguntar" y carpeta por ambiente. Destinos ssh (con bastion y `rsync`) y docker context, y `baton config` que guarda y prueba conexiones. Logs en JSON y retención. |
| 2026-10-01 | `baton import`: convierte GitHub Actions, GitLab CI y Azure Pipelines en un plan. Gestores de secretos (Vault, Azure Key Vault). Helpers para scripts bash (`baton select`, `confirm`, `input`), la etiqueta del proyecto en el prompt, `baton start`/`create`, la página de manual y `scripts/install.sh`. |
| 2026-10-02 | Pasos `script` y `sql` (PostgreSQL), con confirmación ante sentencias destructivas, respaldo de la base y restauración en el rollback. Historial de ejecuciones y visor de logs dentro de la aplicación. |
| 2026-10-07 | **0.1.0.** CI en Linux y macOS, copiar, renombrar y eliminar planes, licencia MIT, guía para asistentes de IA e instalación con Homebrew. |
| 2026-10-07 | **0.2.0.** `baton update` y binarios en cada release. `{ambiente}` en los comandos. `baton last` y `baton history`, y un resumen visual en `baton`. Llaves ssh con frase secreta y bastion con llave propia. Exportación de logs a OTLP y syslog. |
| 2026-10-07 | **0.3.0.** Varias bases de datos por plan y SQLite junto a PostgreSQL. Probar la conexión de una credencial `db`. "Abrir shell" en el destino del paso que falló. `baton db`: consultas de solo lectura contra las bases del plan, en tabla, CSV o JSON. |
| 2026-10-07 | **0.4.0.** MySQL y MariaDB. Sesión interactiva de `baton db`, con historial y comandos para ver tablas y columnas. Instalador de una línea y paquetes `.deb` y `.rpm`. |
| 2026-10-08 | **0.5.0.** Diseño nuevo de la interfaz: ayuda con `?`, filtro `/` y panel del paso en la vista del plan, ejecución con una celda por paso, resumen con barras de tiempo y teclas `1` a `9` en el fallo. El rollback muestra antes lo que deshace. Ambientes protegidos (`[ambientes.<nombre>] protegido = true`) que piden escribir su nombre. `baton db` muestra un registro por bloque cuando la tabla no cabe. |
| 2026-10-08 | **0.6.0.** Plugins: un tipo de paso nuevo (`terraform`, `make`, el que uses) descrito con un manifiesto TOML, sin código y para cualquier herramienta. `baton plugin new`, `validate`, `add`, `list` y `remove`. `add` solo instala un commit con firma verificada por GitHub, fija su hash y no carga un plugin que cambió después de instalarlo. `--dry-run` ejecuta de verdad el comando de solo lectura del tipo, y antes de aplicar se pide confirmar si el plan destruye algo. Campo `dry_run` por paso, credenciales que declara el plugin con los nombres de variable de su herramienta, y una guía para quien escribe uno ([docs/plugins.md](docs/plugins.md)). |

## Desarrollo

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

El código es un workspace de Rust en `crates/` (`baton-core`, `baton-store`, `baton-exec`, `baton-tui`, `baton-cli`).

## Licencia

MIT. Ver [LICENSE](LICENSE).
