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

Si un cambio agrega una conexión nueva o altera una de estas garantías, debe quedar escrito en el
PR y reflejado en esta tabla.

## Versiones con soporte

Solo la última versión publicada recibe correcciones de seguridad.
