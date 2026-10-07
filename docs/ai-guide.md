# Guía para IAs: cómo generar un plan de baton

Este documento está escrito para que lo lea un asistente de IA (Claude Code, Gemini, DeepSeek, ChatGPT u otro). Si una persona te lo entregó junto con su proyecto, tu tarea es **escribir un plan de baton** para ese proyecto. Léelo completo antes de escribir nada.

## Qué es baton

baton es una herramienta de terminal que ejecuta, en orden, una lista de pasos de instalación o despliegue (docker compose, Dockerfile, scripts sh, SQL) y muestra el progreso. Un **plan** es esa lista. Vive en un archivo TOML dentro del proyecto y se versiona con el código.

## Qué debes entregar

Un solo archivo: `baton/plans/<nombre>.toml`, en la raíz del proyecto.

- `<nombre>` en minúsculas, números, `-` y `_` (por ejemplo `instalar`, `deploy-prod`).
- La línea `name = "<nombre>"` del archivo **debe coincidir** con el nombre del archivo.
- Escribe solo ese archivo. No toques `.baton/` ni crees credenciales (ver "Lo que nunca debes hacer").

## Cómo trabajar

1. **Inspecciona el proyecto.** Busca `docker-compose.yml` / `compose.yml`, `Dockerfile`, scripts `.sh` y archivos `.sql`. Lee los compose para ver servicios, puertos y `healthcheck`. Lee la cabecera de los scripts (el `#!` y qué asumen).
2. **Pregunta lo que no puedas deducir.** Por ejemplo: en qué orden deben correr los scripts, si algún paso es destructivo, si hay que respaldar volúmenes, si el despliegue es local o remoto.
3. **Propón los pasos en el orden de ejecución.** Los pasos corren **en secuencia, en el orden del archivo**. Primero lo preparatorio, después construir, después levantar servicios, al final las verificaciones.
4. **Escribe el archivo y valídalo** (ver "Cómo comprobar tu trabajo").

## Estructura del archivo

El orden dentro del archivo importa en TOML: primero las claves sueltas, luego las tablas.

```toml
name = "instalar"                  # obligatorio, igual al nombre del archivo
description = "Qué hace el plan"   # opcional

[options]                          # opcional; todo es false por defecto
backup = true                      # interruptor general de respaldos
auto_rollback = false              # deshacer solo si el plan falla o se aborta
dry_run = false

[backup]                           # solo si usas respaldos
volumes = ["pg_data"]              # volúmenes docker a respaldar
database = false                   # true: además vuelca la base (necesita credencial db); o una lista de ids: ["app", "reportes"]
dir = ".baton/backups"             # opcional

[[steps]]                          # uno por paso, en orden de ejecución
# ... ver abajo

[[credentials]]                    # al final del archivo, solo si hacen falta
# ... ver abajo
```

**Importante:** el parser rechaza cualquier campo que no esté en esta guía (`deny_unknown_fields`). No inventes campos.

## Pasos (`[[steps]]`)

Campos de un paso:

| Campo | Obligatorio | Descripción |
|---|---|---|
| `id` | sí | Identificador único: minúsculas, números, `-`, `_` (`base-de-datos`). No cambies un `id` ya publicado. |
| `name` | sí | Nombre legible para la pantalla. |
| `type` | sí | `compose`, `dockerfile`, `script`, `sql`, `comando`, `check`, `backup` o `gate`. |
| `description` | no | Una línea gris bajo el nombre. |
| `enabled` | no | `true` por defecto. Usa `false` para pasos que existen pero no deben correr solos. |
| `source` | según tipo | Archivo, glob o lista de ellos: `"db/*.sql"`, `["api/Dockerfile", "web/Dockerfile"]`. |
| `command` | según tipo | Comando que se ejecuta (`sh -c`). |
| `depends_on` | no | Lista de `id` de pasos **anteriores** que deben haber corrido. |
| `target` | no | Nombre de un destino definido en `.baton/config.toml`. Omítelo salvo que la persona te dé el nombre; `"local"` siempre existe. |
| `timeout` | no | Duración: `30s`, `5m`, `1h`. Por defecto `5m`. |
| `retries` | no | Reintentos automáticos (entero, por defecto 0). |
| `rollback` | no | Comando que deshace el paso. |
| `backup_before` | no | `true` respalda antes de este paso (requiere `[backup]`). |
| `gate` | no | Tabla `[steps.gate]` (ver "Gates"). |

### Qué exige cada tipo

| `type` | Requiere | Comportamiento |
|---|---|---|
| `compose` | `source` | Corre una vez **por archivo**, dentro de la carpeta del archivo. Sin `command`: `docker compose up -d`. |
| `dockerfile` | `source` | Igual. Sin `command`: `docker build -t {name}:latest .` |
| `script` | `source` | Un `.sh` por archivo, en orden alfabético, dentro de su carpeta. Sin `command` usa el shebang (`#!/bin/bash`, o `sh` si no hay). No necesita permiso de ejecución. |
| `sql` | `source`, y una credencial `db` (PostgreSQL) o `sqlite` | Ejecuta cada `.sql` con `psql` o `sqlite3`. El primer error detiene todo. |
| `comando` | `command` | Un comando suelto, en la raíz del proyecto. |
| `check` | `command` | Una verificación: pasa si el comando termina con código 0. |
| `backup` | sección `[backup]` | Respalda los volúmenes (y la base si `database = true`). Sin `[backup]` el plan no valida. |
| `gate` | tabla `[steps.gate]` | Paso sin acción, solo una pausa o verificación. **No puede** llevar `source`, `command` ni `rollback`. |

### Reglas de `source`

- Siempre **relativo a la raíz del proyecto**.
- **Nunca** absoluto, ni con `..`, ni apuntando a `.baton/`.
- Un glob debe coincidir con archivos existentes, o la ejecución se rechaza antes de empezar.

### Placeholders en `command` y `rollback`

Entre llaves, para pasos `compose`, `dockerfile`, `script` y `sql`: `{plan}` `{fecha}` `{destino}` `{ambiente}` (el que elige quien ejecuta con `--ambiente`; sin ambiente el plan no corre) `{file}` (ruta del archivo) `{dir}` (su carpeta) `{name}` `{script}` (nombre del archivo con extensión) `{stem}` (sin extensión). En los demás tipos solo `{plan}`, `{fecha}`, `{destino}` y `{ambiente}`. Un `{nombre}` desconocido es advertencia, no error.

Como cada comando corre **dentro de la carpeta de su archivo**, un script que asume la raíz del proyecto debe volver a ella (`cd "$(dirname "$0")/.." || exit 1`). Si lo ves en un script, no lo cambies tú; menciónalo.

### Orden y dependencias

- Los pasos corren en el orden del archivo. `depends_on` no reordena: solo decide qué se omite si una dependencia no corrió.
- `depends_on` solo puede apuntar a pasos que están **antes** en el archivo. Una dependencia hacia adelante, a sí mismo o circular es error.
- Encadena con `depends_on` cuando un paso necesite de verdad al anterior (por ejemplo, migraciones después de la base).

## Gates (esperar antes de seguir)

Un gate es una condición para avanzar. Va como `[steps.gate]` bajo cualquier paso (se evalúa después de ejecutarlo), o como un paso de `type = "gate"` si solo quieres la pausa.

**Manual**: pide confirmación a la persona.

```toml
[steps.gate]
mode = "manual"
message = "¿Todo bien para continuar?"
```

**Automático**: espera a que pasen verificaciones.

```toml
[steps.gate]
mode = "auto"
condition = "all"      # all | at_least | critical
# at_least = 2         # solo con condition = "at_least"
# timeout = "60s"      # por check
# attempts = 6         # por check
# parallel = true
```

- En un paso `compose` o `dockerfile`, un gate automático **sin `checks` los infiere solo**: espera el `healthcheck` del servicio si lo define; si no, un HTTP a `http://{destino}:{puerto}/health` con el primer puerto expuesto; si no, que el contenedor siga arriba 30 s. Es la opción más simple: úsala salvo que tengas una razón.
- En un paso `script`, `sql` o `comando`, un gate automático **necesita `checks` explícitos** (solo `http` o `command`).
- Checks explícitos:

```toml
[[steps.gate.checks]]
kind = "http"                      # healthcheck | http | command | running
name = "API responde"              # nombre libre
url = "http://{destino}:8080/health"
critical = true                    # solo importa con condition = "critical"

[[steps.gate.checks]]
kind = "command"
name = "Migraciones aplicadas"
run = "test -f /tmp/ok"
```

  - `http` necesita `url`; `command` necesita `run`; `healthcheck` y `running` necesitan `service` (nombre del servicio en el compose) y solo valen en pasos `compose` o `dockerfile`.
  - `condition = "critical"` exige que al menos un check lleve `critical = true`; `"at_least"` exige `at_least` (entero, no mayor que los checks activos).
  - Un gate manual no lleva checks; `message` solo se usa en gates manuales.

## Credenciales (`[[credentials]]`)

Declara una solo si el plan la necesita: un paso `sql` (o `[backup] database = true`), un registro docker privado, git con token. **Nunca escribes valores**, solo una referencia.

```toml
[[credentials]]
id = "db"                  # único en el plan
kind = "db"                # git | docker | ssh | db | sqlite | otro
label = "Base de datos"    # opcional
ref = "db.env#DB"          # archivo.env#PREFIJO
```

- `ref` tiene la forma `archivo.env#PREFIJO`: archivo en minúsculas (`db.env`, `docker.env`), prefijo en MAYÚSCULAS (`DB`, `GHCR`).
- Cada tipo tiene campos fijos que baton pide al ejecutar y guarda en `.baton/credentials/` (fuera del código). Con `ref = "docker.env#GHCR"` las variables son `GHCR_REGISTRY`, `GHCR_USER` y `GHCR_TOKEN`; con `db.env#DB`, `DB_USER`, `DB_PASSWORD` y, opcionales, `DB_HOST`, `DB_PORT`, `DB_DATABASE`, `DB_CONTAINER`.
- Un paso solo recibe en su entorno las variables que menciona (`$GHCR_TOKEN`), salvo `compose`, `dockerfile`, `script` y `sql`, que reciben todas las del plan.
- Solo puede haber **una** credencial `db` por plan.

## Ejemplo completo

Proyecto con una base de datos, una API, migraciones y scripts. Todo este plan está comprobado con `baton validate`.

```toml
name = "instalar"
description = "Instala el stack: requisitos, base de datos, API y verificación"

[options]
backup = true
auto_rollback = false

[backup]
volumes = ["pg_data"]

[[steps]]
id = "requisitos"
name = "Verificar requisitos"
type = "script"
source = "scripts/01-requisitos.sh"
timeout = "1m"

[[steps]]
id = "respaldo"
name = "Respaldar volúmenes"
type = "backup"
depends_on = ["requisitos"]

[[steps]]
id = "base-de-datos"
name = "Levantar la base de datos"
type = "compose"
source = "services/db/docker-compose.yml"
command = "docker compose up -d --wait"
depends_on = ["respaldo"]
timeout = "5m"
retries = 1
rollback = "docker compose down"

[steps.gate]
mode = "auto"
condition = "all"

[[steps]]
id = "migraciones"
name = "Aplicar migraciones"
type = "sql"
source = "db/*.sql"
depends_on = ["base-de-datos"]

[[steps]]
id = "api"
name = "Levantar la API"
type = "compose"
source = "services/api/docker-compose.yml"
depends_on = ["migraciones"]
rollback = "docker compose down"

[steps.gate]
mode = "auto"
condition = "all"

[[steps.gate.checks]]
kind = "http"
name = "API responde"
url = "http://{destino}:8080/health"
critical = true

[[steps]]
id = "smoke"
name = "Pruebas de humo"
type = "script"
source = "scripts/02-smoke.sh"
depends_on = ["api"]

[[steps]]
id = "confirmar"
name = "Confirmar puesta en producción"
type = "gate"
depends_on = ["smoke"]

[steps.gate]
mode = "manual"
message = "¿La instalación se ve bien?"

[[credentials]]
id = "db"
kind = "db"
label = "Base de datos"
ref = "db.env#DB"
```

## Lo que nunca debes hacer

- **No escribas secretos** (contraseñas, tokens, llaves) en el plan ni en ningún archivo del proyecto. Solo referencias `[[credentials]]`.
- **No crees ni edites `.baton/`.** Es local de cada máquina (destinos, credenciales, logs) y nunca se versiona ni se envía a los servidores.
- **No pongas ramas por ambiente** (dev, staging, prod) dentro de un plan. Un plan es una lista fija de pasos. Si dos ambientes necesitan pasos distintos, son **dos planes** (`instalar-dev.toml`, `instalar-prod.toml`). Las credenciales sí cambian por ambiente, pero eso lo elige quien ejecuta (`--ambiente`, la variable `BATON_AMBIENTE` o `ambiente` en `[defaults]` de `.baton/config.toml`). Si un comando necesita el nombre del ambiente (una URL, un `--env`), usa `{ambiente}` en vez de una rama.
- **No inventes destinos.** Usa solo `local` u omite `target`, a menos que la persona te diga qué destinos tiene.
- **No actives pasos destructivos por tu cuenta** (limpiar, borrar, `down`, resetear base de datos). Déjalos con `enabled = false` y dilo.
- **No inventes campos ni tipos** que no estén en esta guía.
- **No modifiques los scripts, compose o SQL del proyecto** para que "encajen" con el plan. Si algo no encaja, avisa.

## Cómo comprobar tu trabajo

Con baton instalado, desde la raíz del proyecto:

```sh
baton validate instalar            # revisa el plan y lista todos los errores
baton run instalar --dry-run       # simula la ejecución completa, sin tocar nada
```

`validate` indica el campo exacto de cada error (por ejemplo `steps[2].depends_on[0]: depende de 'x', que no existe`). Corrígelos todos y vuelve a validar hasta ver `todo en orden`. `--dry-run` no ejecuta nada y no deja rastros; además comprueba que cada `source` encuentre archivos y que no falten datos para ejecutar.

Si no puedes ejecutar comandos, repasa esta lista antes de entregar:

- [ ] El archivo está en `baton/plans/<nombre>.toml` y `name` coincide con el archivo.
- [ ] Cada `id` es único, en minúsculas, y cada `depends_on` apunta a un paso anterior.
- [ ] Todo paso `compose`, `dockerfile`, `script` y `sql` tiene `source` relativo y existente.
- [ ] Todo paso `comando` y `check` tiene `command`.
- [ ] Hay `[backup]` si usas un paso `backup` o `backup_before`.
- [ ] Hay una credencial `db` o `sqlite` si usas `sql`; con varias, cada paso `sql` lleva `database = "<id>"`.
- [ ] Un gate automático en un paso que no es compose o dockerfile lleva `checks` explícitos.
- [ ] No hay secretos, ni campos inventados, ni pasos destructivos activos.
- [ ] `[[credentials]]` va al final del archivo, después de los pasos.

## Otros comandos útiles

- `baton init` escanea la carpeta y arma un borrador de plan (sirve de punto de partida).
- `baton import <archivo>` convierte un pipeline de GitHub Actions, GitLab CI o Azure Pipelines en un plan.
- `baton --help` y `man baton` describen todo lo demás.
