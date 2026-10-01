#!/usr/bin/env bash
# Ejemplo de los helpers de baton dentro de un script.
#
#   ./desplegar.sh                     pregunta todo en la terminal
#   BATON_AMBIENTE=staging BATON_QUE_INSTALAR=postgres,nginx BATON_DESPLEGAR=yes ./desplegar.sh
#                                      no pregunta nada (CI): cada respuesta sale de su variable
#
# Cada helper dibuja su pregunta en la terminal y solo imprime el resultado por stdout, así que
# `$(...)` lo captura. Sin terminal ni variable, falla diciendo cuál falta.
set -euo pipefail

ambiente=$(baton select "¿Ambiente?" dev staging prod --default dev)

# uno por línea: se recorre con `while read`, a salvo de espacios
servicios=$(baton multiselect "¿Qué instalar?" postgres redis nginx --default postgres --min 1)

nombre=$(baton input "Nombre del stack" --default "demo-$ambiente")

echo "ambiente: $ambiente"
echo "stack:    $nombre"
while IFS= read -r servicio; do
  echo "instalar: $servicio"
done <<<"$servicios"

# `confirm` responde con el código de salida (0 sí, 1 no, 130 cancelado)
if baton confirm "¿Desplegar a $ambiente?" --name desplegar --default no; then
  echo "desplegando..."
else
  echo "no se despliega nada"
fi
