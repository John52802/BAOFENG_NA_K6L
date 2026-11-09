# BAOFENG NA-K6: firmware modificado

Versión no oficial del firmware para radios Baofeng NA-K6. Probada solo en radios NA-K6 con chip de radio FD6818. Baofeng no la respalda.

## Repositorio base

Este trabajo se basa en **[cnt7/BAOFENG-UV-K6-Firmware](https://github.com/cnt7/BAOFENG-UV-K6-Firmware)**.

## ⚠️ Antes de instalar

- **Solo para NA-K6 (procesador KD32F328) con chip de radio FD6818.** Con el chip FD6818B la parte de radio deja de funcionar. Para saber cuál lleva el tuyo hay que abrir el radio y leer el chip. No sirve para radios Quansheng.
- **No se puede volver atrás.** Baofeng no publica el firmware de fábrica, y no se puede leer del radio. Instalas esto bajo tu propio riesgo.
- **Respeta la normativa de radio.** Este firmware mantiene los rangos de transmisión del original, que son muy amplios (hasta 600 MHz). Transmite solo en las frecuencias que tengas autorizadas. El firmware bloquea la transmisión en banda aérea.
- **Haz una copia de tu radio** antes de modificar sus voces o canales. Cada radio tiene su propia calibración: no restaures la copia de un radio en otro.

## Qué añade

- Recepción de 100 a 999,999 MHz, con banda aérea en AM. Por encima de 600 MHz puede no recibirse nada: el hardware no está pensado para eso.
- AM opcional en cualquier frecuencia (mantener `2`).
- Textos en español: `MENU` > `Language` > `Español`.
- Voces en español en una zona libre de la memoria flash, sin borrar las de fábrica. El código fuente no incluye los audios; el `.exe` de las [releases](https://github.com/John52802/BAOFENG_NA_K6L/releases) sí.
- Vista aproximada de espectro (mantener `1` o pulsar `BAND`). Indica actividad, no una frecuencia exacta: el error medido fue de unos 275 kHz en el ancho de 2 MHz y de unos 3 MHz en el de 10 MHz. Con `MENU` se acerca al pico y, en el ancho de 2 MHz, busca la señal y la escucha sin cambiar el VFO.
- En modo canal, con la pantalla en Nombre y sin doble escucha, oculta la frecuencia y muestra un porcentaje aproximado de batería. Solo la oculta de la vista: no da privacidad en el aire.
- Sin doble escucha, muestra los segundos que llevas transmitiendo y una barra relativa de señal al recibir, y el porcentaje también en modo VFO.
- El silenciador se respeta por debajo de 130 MHz, y el escaneo por frecuencia ya no graba en la flash a cada paso.

## Fallos corregidos del firmware base

- Quedarse transmitiendo sin parar al enviar tonos DTMF.
- Cuelgue en el menú `FM BAND` en modo canal, y quedar atrapado en el menú FM mientras se recibe.
- Desplazamientos del menú guardados multiplicados por 10, que hacían transmitir en otra frecuencia.
- Transmisión en frecuencias no permitidas usando la inversión.
- Código DCS al aire distinto del que muestra la pantalla, y polaridad de recepción tomada del tono de transmisión.
- Tonos de RX y TX intercambiados al guardarlos en modo VFO.
- Logo y mensaje de encendido que nunca llegaban a verse.
- Pantalla de clave de encendido inutilizable: las teclas se repetían solas y los números no se veían.
- Esperas del firmware (`DelayMs`, `DelayUs`) que el compilador eliminaba, de modo que no esperaban nada.
- Restos de la ventana de DTMF mezclados con la pantalla al transmitir.

## Compilar el firmware

Hace falta ARM GCC (`arm-none-eabi-gcc`). La lista de archivos y las opciones de compilación están en el `Makefile`, y el mapa de memoria en `kd32f328xb.ld`. El firmware ya compilado solo está dentro del `.exe` de las [releases](https://github.com/John52802/BAOFENG_NA_K6L/releases).

## Instalar el firmware

Con el cable de programación conectado, enciende el radio con los 2 botones laterales naranjas pulsados para entrar en modo `UPDATE`. Después carga el archivo `.bin` con `BFK6_Bootloader.exe` o con el programa para Windows de este repositorio.

## Programa para Windows

`k6_actualizar.exe` saca copias de la memoria flash, instala firmware, cambia voces, edita canales con sus tonos CTCSS y DCS, potencia, ancho de banda y demás opciones, edita los VFO A y B y las emisoras de FM, restaura copias, guarda y aplica perfiles completos, tiene modo oscuro, y cambia los ajustes del radio, la clave, el mensaje y el logo de encendido. Solo escribe en las zonas de voces, canales y logo de la memoria flash externa, los ajustes los guarda el propio firmware, y nunca borra el chip completo. Se descarga ya compilado desde las [releases](https://github.com/John52802/BAOFENG_NA_K6L/releases), con el firmware y las voces en español incluidos.

Para compilarlo desde `tools/k6-actualizar`:

```
cd tools/k6-actualizar
cargo build --release
```

Si existen `out/bf_uvk6-rx-spectrum.bin` y `out/voces_es_gonzalo/voces_es.bin`, los incluye en el programa. Si no, elige el firmware desde un archivo `.bin`, y no ofrece las voces en español.

## Licencia

Este trabajo deriva del código abierto de Baofeng y se distribuye bajo el acuerdo de [LICENSE](LICENSE) (BFPL-1.0): uso personal y no comercial, conservando el acuerdo y el aviso de copyright, y cumpliendo la normativa de radio.
