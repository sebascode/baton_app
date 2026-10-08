# Seguridad

## Reportar una vulnerabilidad

No abras un issue público. Usa el reporte privado de GitHub: pestaña **Security**, botón
**Report a vulnerability**. Responderé lo antes posible y coordinaremos la publicación del arreglo.

Interesa en especial cualquier camino por el que una credencial, una llave, el contenido de
`.baton/` o datos de un servidor puedan salir de la máquina sin que la persona lo haya pedido.

## Qué sale de la máquina

Baton no tiene telemetría, no envía estadísticas de uso y no reporta errores. Estas son todas las
conexiones que puede hacer, y cada una la inicia la persona o está configurada por ella:

| Qué | A dónde | Cuándo |
|---|---|---|
| `baton plugin add github:...` | `api.github.com` y `raw.githubusercontent.com`, con `curl` (solo https) | Solo si se ejecuta el comando, en una terminal y tras mostrar los comandos del plugin |
| `baton update` | `github.com/sebascode/baton_app` (releases), con `curl` | Solo si se ejecuta el comando |
| `ssh`, `rsync` | Los destinos ssh declarados en `.baton/config.toml` | Al ejecutar un paso en ese destino |
| `docker` con un contexto | El contexto elegido en el destino | Al ejecutar un paso en ese destino |
| Exportación de logs (OTLP, syslog) | El endpoint de `[logs.export]` | Solo si se configuró; apagada por defecto |
| Proveedores de secretos | Lo que haga el comando que la persona escribió en `[secrets.*]` | Al resolver una credencial |
| Prueba de credencial `docker` | El registro de esa credencial | Al pulsar probar |

Garantías que el código mantiene y que una revisión debe proteger:

- `.baton/` nunca viaja a un destino.
- Los secretos no van en argumentos de procesos: viajan por el entorno del proceso, o por la
  entrada estándar en ssh.
- Los valores de campos secretos se tachan en todo lo que se muestra o se guarda.
- Un comando solo recibe las credenciales que menciona.

Plugins (`baton plugin add`):

- Solo se instala el commit de un repositorio de GitHub cuya firma GitHub haya verificado, y el
  manifiesto se baja de ese SHA exacto, no de un tag que pueda moverse.
- Nada se instala sin que una persona, en una terminal, vea antes los comandos que va a ejecutar.
- `plugins.lock` guarda el origen, el commit y el sha256 de lo instalado; un manifiesto que cambia
  después ya no se carga, y si el registro no se puede leer no se carga ninguno.
- Una firma verificada prueba de quién es el commit, no que sus comandos sean inofensivos.
- Un plugin recibe una credencial solo si el plan la declara, y solo las variables que su manifiesto
  mapea para ese tipo; `baton plugin add` muestra cuáles antes de instalar. Un manifiesto no puede
  definir variables como `PATH`, `HOME`, `LD_*` o `BASH_ENV`, y los valores secretos se tachan de
  todo lo que se muestra o guarda.

Si un cambio agrega una conexión nueva o altera una de estas garantías, debe quedar escrito en el
PR y reflejado en esta tabla.

## Versiones con soporte

Solo la última versión publicada recibe correcciones de seguridad.
