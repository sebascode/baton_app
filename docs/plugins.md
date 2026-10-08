# Escribir un plugin para baton

Un plugin añade un **tipo de paso** nuevo a baton (`terraform`, `make-build`, el que tú inventes),
junto a `compose`, `script` o `sql`. Es un archivo de texto, `baton-plugin.toml`, que describe
**qué comando ejecutar** y qué necesita. No tiene código: baton se queda con la ejecución, las
credenciales, los logs, los gates y la pantalla.

Eso lo hace independiente del lenguaje. Un plugin envuelve una **herramienta** que se invoca como
un comando (`terraform`, `make`, `npm`, `cargo`, `kubectl`, un binario tuyo escrito en Go, un script
Python instalado en el PATH...). Cómo esté escrita esa herramienta no le importa a baton.

Esta guía sirve igual para una persona que para un asistente de IA: todo lo que dice se comprueba
con `baton plugin validate`.

## Qué puede y qué no puede un plugin

Un plugin **puede**:

- Definir un tipo de paso con un comando por defecto, que corre una vez por carpeta o una sola vez.
- Declarar un `dry_run` de solo lectura (`terraform plan`, `make --dry-run`) que `--dry-run` ejecuta
  de verdad.
- Pedir que baton confirme antes de ejecutar si el `dry_run` muestra que algo se destruye.
- Decir qué archivos reconoce (`**/main.tf`) para que `baton init` proponga el paso.
- Exigir programas instalados (`requires`) y comprobarlos antes de ejecutar nada.
- Usar credenciales de baton y entregarlas con los nombres de variable que su herramienta espera.

Un plugin **no puede**:

- **Traer archivos propios.** Se instala solo el manifiesto. Si necesitas lógica, ponla en una
  herramienta que se instala aparte (un paquete, un binario) y llámala desde `command`; declárala
  en `requires` para que baton avise si falta.
- Ejecutar nada al instalarse: instalar un plugin solo copia un archivo.
- Redefinir un tipo de baton (`compose`, `script`...), ni exigir backup o gate.
- Definir variables como `PATH`, `HOME`, `SHELL`, `LD_*` o `BASH_ENV` al entregar una credencial.
- Cambiar la pantalla de baton.

## Tu primer plugin en cinco minutos

Vamos a envolver `make`: un tipo de paso que compila cada carpeta que tenga un `Makefile`.

**1. Crea el esqueleto.**

```sh
baton plugin new make-build
```

Crea `make-build/baton-plugin.toml` con un manifiesto comentado para empezar.

**2. Escribe el manifiesto.** Reemplaza su contenido:

```toml
api = 1
name = "make-build"
version = "0.1.0"
description = "Compila con make cada carpeta que tenga un Makefile"
requires = ["make"]

[type]
scanned = true
detect = ["**/Makefile"]
command = "make"
dry_run = "make --dry-run"
```

`scanned = true` quiere decir que el `source` del paso es un glob y el comando corre **una vez por
archivo, dentro de su carpeta**. `dry_run` es lo que se ejecuta con `baton run --dry-run`: `make
--dry-run` imprime lo que haría sin hacerlo.

**3. Revísalo.**

```sh
baton plugin validate make-build
```

Muestra el comando, el dry-run y los programas que exige, y señala con `archivo:línea:columna` lo
que esté mal.

**4. Instálalo en tu máquina.**

```sh
baton plugin add ./make-build
```

Te muestra lo que va a ejecutar y pregunta. Necesita una terminal. Para ver los instalados:
`baton plugin list`.

**5. Úsalo en un plan.** En `baton/plans/construir.toml`:

```toml
name = "construir"

[[steps]]
id = "compilar"
name = "Compilar"
type = "make-build"
source = "app/Makefile"
```

**6. Pruébalo.**

```sh
baton validate construir
baton run construir --dry-run     # ejecuta make --dry-run en app/
baton run construir
```

Si tu proyecto ya tiene `Makefile`, `baton init` propondrá el paso solo, **desactivado**: un
escaneo nunca activa por su cuenta un paso que puede modificar cosas.

## Referencia del manifiesto

El formato es `api = 1`. Un campo que baton no conoce es un error de lectura (así un error de
tipeo no pasa en silencio). Un manifiesto no puede pesar más de 64 KB.

### Nivel superior

| Campo | Obligatorio | Qué es |
|---|---|---|
| `api` | sí | Versión del formato. Hoy `1`. |
| `name` | sí | Nombre del plugin: minúsculas, números y guiones. Es el de su carpeta. |
| `version` | sí | `X.Y.Z`. |
| `description` | no | Una línea. Se ve en `baton plugin list`. |
| `requires` | no | Programas que tienen que existir donde corre el paso (`["make"]`). Se comprueban antes de ejecutar. En un destino ssh no se comprueban. |

### `[type]`

| Campo | Obligatorio | Qué es |
|---|---|---|
| `name` | no | Cómo se escribe en el plan (`type = "..."`). Por defecto, el nombre del plugin. No puede ser un tipo de baton. |
| `command` | sí | El comando por defecto. Un paso puede declarar el suyo. |
| `scanned` | no | `true`: el `source` es un glob y el comando corre una vez por archivo, dentro de su carpeta. `false` (por defecto): corre una vez y el paso lleva su `command` si quiere otro. |
| `detect` | no | Globs relativos al proyecto con los que `baton init` propone el paso. |
| `dry_run` | no | Comando de **solo lectura** para `--dry-run`. Ver más abajo. |
| `destructive` | no | Frases que, si aparecen en la salida del `dry_run`, piden confirmar antes de ejecutar. Necesita `dry_run`. |

Dentro de `command` y `dry_run` puedes usar estas variables entre llaves: `{plan}`, `{fecha}`,
`{destino}`, `{ambiente}`, `{file}`, `{dir}`, `{name}`, `{script}` y `{stem}`. Con `scanned = true`,
`{file}` es el archivo, `{dir}` su carpeta y `{name}` el nombre de esa carpeta. Una variable que
baton no conoce es una advertencia: se deja tal cual, porque un comando puede llevar llaves
legítimas.

### `[[credentials]]`

Ver la sección de credenciales más abajo.

## Escribir el `dry_run`

Con `baton run --dry-run`, baton normalmente no ejecuta nada. Un tipo con `dry_run` es la excepción:
ejecuta ese comando de verdad, así que **tiene que ser de solo lectura**. Es lo que te deja ver qué
haría el paso sin que lo haga.

- Bien: `terraform plan`, `az deployment group what-if`, `make --dry-run`, `npm ci --dry-run`,
  `docker compose config`.
- Mal: `terraform apply`, `kubectl apply`, `docker compose up`.

baton rechaza un `dry_run` igual a `command` o que use palabras como `apply`, `destroy`, `create`,
`deploy`, `up`, `rm` o `push`. **Es un chequeo de apariencia, no una garantía**: un comando puede
esconder lo que hace. La garantía real es que quien instala tu plugin lee los comandos antes de
aceptarlos. Escríbelos para que se entiendan.

Un paso del plan puede declarar su propio `dry_run` (y su propio `command`); el del plan manda
sobre el del tipo.

## Pedir confirmación ante lo destructivo

Si `destructive` lista frases, baton hace esto **antes de ejecutar** un paso de tu tipo:

1. Ejecuta el `dry_run` de todas las carpetas y guarda su salida.
2. Busca esas frases (texto literal, sin distinguir mayúsculas ni colores).
3. Si hay coincidencias, las escribe en el log y pregunta una sola vez. Rechazar detiene el plan.
4. Sin terminal y sin `--assume-yes`, el paso falla antes de ejecutar nada.

```toml
api = 1
name = "ejemplo-destructivo"
version = "0.1.0"
requires = ["herramienta"]

[type]
scanned = true
command = "herramienta aplicar"
dry_run = "herramienta planificar"
destructive = ["se destruirá", "será reemplazado"]
```

Elige frases que solo aparezcan cuando algo se borra. Por ejemplo, el resumen de Terraform dice
`0 to destroy` cuando no destruye nada: la frase `to destroy` coincidiría siempre. Usa la que
Terraform imprime por recurso: `will be destroyed`.

Si un paso reescribe `command`, baton exige que declare también su propio `dry_run`: si no, la
revisión de lo destructivo se perdería sin que nadie lo notara.

## Credenciales

Muchas herramientas leen sus credenciales de variables de entorno con nombres fijos (Terraform
espera `AWS_ACCESS_KEY_ID`). Tu manifiesto declara qué credencial usa y con qué nombres la recibe:

```toml
api = 1
name = "ejemplo-nube"
version = "0.1.0"
requires = ["nube"]

[type]
command = "nube desplegar"
dry_run = "nube simular"

[[credentials]]
kind = "mi-nube"
fields = [
  { key = "TOKEN", secret = true },
  { key = "REGION", optional = true },
]

[credentials.env]
MI_NUBE_TOKEN = "{TOKEN}"
MI_NUBE_REGION = "{REGION}"
```

- `kind` es el nombre del tipo de credencial (minúsculas, números y guiones). Si no es de baton,
  `fields` lo define.
- Cada elemento de `fields` tiene:

  | Campo | Obligatorio | Qué es |
  |---|---|---|
  | `key` | sí | Nombre del campo en mayúsculas, números y `_`, empezando por letra (`ACCESS_KEY_ID`). |
  | `label` | no | Cómo se llama en el formulario. Por defecto, la clave en minúsculas. |
  | `secret` | no | `true`: se enmascara en pantalla y se tacha de todo lo que baton muestra o guarda. |
  | `optional` | no | `true`: puede quedar vacío. |

- `[credentials.env]` dice con qué variables llega cada valor: `VARIABLE = "plantilla con
  {CAMPOS}"`. Una variable cuya plantilla usa un campo opcional vacío no se define.
- Con un tipo de baton (`docker`, `git`...) se omite `fields` y se da solo `env`.
- Dos plugins pueden usar el mismo tipo de credencial si lo definen igual. Si lo definen distinto,
  el segundo no se carga.

Quien use tu plugin declara la credencial en su plan, como cualquier otra:

```toml
name = "desplegar"

[[credentials]]
id = "nube"
kind = "mi-nube"
ref = "servers.env#PROD"

[[steps]]
id = "app"
name = "Desplegar"
type = "ejemplo-nube"
```

Reglas que conviene conocer:

- Tu comando recibe **solo** las variables de `env`, y solo si el plan declara la credencial. Si no
  la declara, no recibe nada y la herramienta usa lo que ya haya en el entorno (`aws sso`, un perfil).
- Un plan solo puede declarar una credencial de cada tipo que use el plugin. Para cuentas distintas
  se usan los ambientes (`--ambiente`).
- No puedes definir `PATH`, `HOME`, `SHELL`, `IFS`, `ENV`, `BASH_ENV`, `LD_*`, `DYLD_*` ni `BATON_*`.

## Un ejemplo real

`examples/plugins/terraform` en este repositorio es un plugin completo: detecta `main.tf`, guarda el
plan y aplica ese mismo plan, pide confirmar si algo se destruye y entrega la credencial `aws`.
Léelo antes de escribir el tuyo.

## Publicar tu plugin

Un plugin vive en **su propio repositorio de GitHub**, con el manifiesto en la raíz:

```
mi-plugin/
└── baton-plugin.toml
```

baton solo instala un commit cuya firma GitHub haya **verificado**, y baja el manifiesto de ese
commit exacto, no de un tag que pueda moverse. Para publicar:

**1. Firma tus commits.** Con una llave SSH (la forma más corta):

```sh
git config gpg.format ssh
git config user.signingkey ~/.ssh/id_ed25519.pub
git config commit.gpgsign true
```

Sube esa misma llave pública a GitHub como **llave de firma**, no solo de autenticación:
Settings, SSH and GPG keys, New SSH key, tipo "Signing Key". Después de subir un commit firmado, en
GitHub debe aparecer con la etiqueta **Verified**.

**2. Etiqueta una versión.**

```sh
git tag v0.1.0
git push origin v0.1.0
```

El commit al que apunta el tag es el que debe estar firmado. baton pide siempre una versión (un tag
o un commit), nunca una rama.

**3. Compruébalo como lo haría otra persona.**

```sh
baton plugin add github:tu-usuario/tu-plugin@v0.1.0
```

Si no sale `Verified`, baton se niega y dice por qué (`unsigned`, `unknown_key`...).

**4. Para publicar una versión nueva**, sube la versión en el manifiesto, haz un commit firmado y
crea otro tag. Quien ya lo tenía instalado ejecuta `baton plugin add` de nuevo y baton le muestra
exactamente qué comandos cambiaron antes de pedirle que confirme.

## Qué verá quien lo instale

```
plugin make-build 0.1.0
  origen: github:tu-usuario/tu-plugin@v0.1.0
  commit: 0123456789abcdef0123456789abcdef01234567 (firma verificada por GitHub: valid)
  sha256: ...
  tipo: make-build (una vez por archivo)
  comando: make
  dry-run: make --dry-run
  requiere: make
  detecta: **/Makefile
¿Instalar el plugin 'make-build' 0.1.0? Ejecutará los comandos de arriba en esta máquina (s/N)
```

Esa pantalla es la que decide si confían en tu plugin, así que:

- Mantén los comandos cortos y legibles. Nada de cadenas codificadas ni descargas que se ejecutan
  directamente (`curl ... | sh`).
- Pide solo las credenciales que la herramienta necesita.
- Si algo es destructivo, declara `destructive`.

Una firma verificada prueba **de quién es el commit**, no que sus comandos sean inofensivos. Lo
que protege a quien instala es leerlos.

## Límites actuales

- Se instala solo el manifiesto: no hay archivos de apoyo.
- Una credencial por tipo en cada plan.
- La prueba de conexión de la pantalla de credenciales no existe para los tipos de plugins.
- La revisión de lo destructivo mira el plan de un momento: el comando del paso vuelve a
  planificar, y lo que aplique podría diferir si algo cambió en medio.
- baton corre en Linux y macOS.

## Solución de problemas

| Mensaje | Qué pasa y qué hacer |
|---|---|
| `tipo de paso desconocido 'x'` | El plugin que define `x` no está instalado o no se pudo cargar. `baton plugin list` lo dice. |
| `no tiene una firma verificada por GitHub` | El commit no está firmado, o la llave de firma no está subida a GitHub como "Signing Key". |
| `la carpeta se llama 'a' pero el plugin se llama 'b'` | La carpeta del plugin tiene que llamarse igual que su `name`. |
| `api = N no está soportada` | El manifiesto es de una versión del formato más nueva que tu baton. Actualiza baton. |
| `el plugin cambió desde que se instaló` | Alguien editó el manifiesto instalado. No se carga. Revísalo y vuelve a instalarlo, o quítalo con `baton plugin remove`. |
| `dry_run usa 'apply'` | El `dry_run` parece modificar algo. Usa el comando de solo lectura de tu herramienta. |
| `el tipo 'x' revisa lo destructivo con su dry_run` | Un paso reescribe `command` sin declarar su propio `dry_run`. Añádelo. |
