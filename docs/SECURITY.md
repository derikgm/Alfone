# Seguridad de Alfone

Última actualización: 2026-11-05

Alfone inyecta eventos de teclado y ratón en tu escritorio. Un fallo de
seguridad aquí no es una fuga de datos: es control remoto de la máquina. El
diseño parte de esa premisa.

## 1. Modelo de amenazas

| Amenaza | Mitigación |
|---|---|
| Alguien en la LAN se conecta y controla el PC | TLS 1.3 + pinning de certificado + PIN por sesión |
| Suplantación del servidor (MITM) | Huella SHA-256 fijada o comparación de código corto (TOFU) |
| Reutilización de una sesión antigua | TLS 1.3 con claves efímeras; PIN nuevo por sesión |
| Peer malicioso envía frames gigantes | Límite de 8 MiB y cierre inmediato |
| Peer intenta inyectar en la pantalla equivocada | `EnterScreen`/`LeaveScreen` con `sequence` |
| Acceso no autorizado al portapapeles/archivos | Capacidades explícitas por peer (M2) |
| Robo de la clave privada del dispositivo | Archivo `key.der` con permisos `0600` en Unix |
| Captura silenciosa de entrada | El servidor solo inyecta tras el handshake y el PIN; indicador en la UI |

Fuera de alcance por ahora: atacante con acceso físico al disco, root en
cualquiera de las dos máquinas, y WAN/NAT (se abordará con `iroh`/Tailscale).

## 2. Identidad

Cada instalación genera una vez un certificado autofirmado (`rcgen`) y guarda:

```
<config_dir>/identity/cert.der   # certificado público
<config_dir>/identity/key.der    # clave privada (0600 en Unix)
```

- La **huella** es `SHA-256(cert.der)`, 64 caracteres hex.
- El **código corto** son 40 bits de la huella en Crockford base32, formateado
  `XXXX-XXXX`. Sirve para comparar en voz alta, no como secreto.
- La identidad no cambia entre sesiones. Borrar `identity/` equivale a un
  dispositivo nuevo y obliga a volver a confiar en él.

## 3. Emparejamiento

Dos factores independientes:

1. **Certificado**: el cliente fija la huella del servidor. Si no la conoce,
   TOFU: se muestra la huella/código y el usuario decide. `alfone pair` guarda
   la huella en `security.trusted_peers` y en `client.fingerprint`.
2. **PIN de sesión**: el servidor genera seis dígitos al arrancar y los muestra
   en su terminal. Sin el PIN correcto, el servidor rechaza la conexión antes
   de aceptar entrada. El PIN se compara con `normalize_code`, que ignora
   guiones y mayúsculas.

El PIN protege contra un atacante pasivo que conoce la huella; el pinning
protege contra un servidor falso. Juntos evitan el escenario típico de "alguien
se conecta a mi PC en la oficina".

`--no-pin` desactiva el segundo factor. Solo es aceptable si la huella está
fijada y la red es de confianza.

## 4. Transporte

- QUIC con TLS 1.3; no se acepta TLS 1.2 ni conexiones en claro.
- ALPN `alfone/1`: un cliente que no lo negocie no llega al handshake.
- El verificador de certificados (`FingerprintVerifier`) implementa
  `ServerCertVerifier` y compara la huella observada con la esperada. No se
  usan CA públicas: el certificado es autofirmado a propósito.
- El cliente envía en `HelloBack` la huella que verificó; el servidor la
  compara con la suya. Detecta terminación TLS intermedia mal configurada.
- Los datagramas de audio/vídeo van dentro del mismo túnel TLS.

## 5. Superficie de ataque y límites

- **Frames de control**: máximo 8 MiB. El largo se valida antes de reservar.
- **Datagramas**: el receptor comprueba longitud mínima y `count`/`index` de
  vídeo; un `count == 0` o `index >= count` se descarta.
- **Postcard**: un cuerpo malformado produce un error de decodificación y se
  cierra la conexión; no hay `unwrap` sobre datos del peer en las rutas de red.
- **Backends de entrada**: solo se inyectan eventos tras un handshake completo.
  En X11 el servidor hace grab del teclado/ratón únicamente mientras el control
  está en la pantalla remota.
- **Configuración**: se parsea con `serde` y valores por defecto; un TOML
  inválido no arranca el daemon.
- **Plano de control HTTP**: escucha solo en `127.0.0.1` y exige
  `Authorization: Bearer <token>` en todos los endpoints (`401` si falta). El
  token es aleatorio de 256 bits, se guarda en `[security] http_token` y no se
  devuelve por `/status`. Al ser loopback no hay TLS: el modelo asume que un
  proceso local ya podría leer la config. Pendiente: permisos `0600` explícitos
  para el fichero de configuración y un socket Unix cuando esté disponible.

## 6. Privacidad

- No hay telemetría. Los logs son locales.
- El audio/vídeo (M3/M4) solo se activa si el usuario lo pide y ambas partes
  anuncian la capacidad.
- El cliente conoce la geometría de sus pantallas y la envía; el servidor
  conoce el nombre del dispositivo del cliente.

## 7. Riesgos conocidos (M0)

- **Sin autenticación mutua del cliente**: el servidor autentica su certificado,
  pero no exige certificado de cliente. El PIN es la única prueba de identidad
  del cliente. M1 añadirá un certificado de cliente o una clave derivada del
  emparejamiento.
- **TOFU sin UI**: en la CLI, el usuario debe comparar el código corto a mano.
  La UI mostrará un diálogo bloqueante.
- **PIN en la línea de comandos**: `--pin` puede quedar en el historial del
  shell. Preferir el PIN generado o una variable de entorno.
- **Sin rate limiting**: un atacante puede intentar PINs en bucle. M1 añadirá
  backoff y límite de intentos por IP.
- **`--no-pin`**: desactiva el segundo factor; úsalo solo en redes controladas.
- **Wayland/Windows/macOS**: los backends aún no existen; no hay superficie que
  auditar allí, pero tampoco soporte.

## 8. Buenas prácticas para usuarios

1. Ejecuta `alfone init` una vez por máquina.
2. Empareja con `alfone pair --server IP --pin 123456` en un momento en que
   puedas comparar el código corto.
3. No compartas el PIN por canales persistentes.
4. Usa una red local de confianza; no expongas el puerto 24800 a Internet.
5. Revisa `security.trusted_peers` si cambias de equipo.
6. Para cortar el control remoto, pulsa `Ctrl+Alt+Esc` o cierra la sesión.

## 9. Divulgación

Este es un proyecto personal en desarrollo. Si encuentras un fallo, abre un
issue privado o contacta al mantenedor antes de publicarlo. No hay programa de
bug bounty.
