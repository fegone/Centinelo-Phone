# DECISION — Consola BLF premium como panel dentro de la ventana principal

Fecha: 2026-02 (rama `feat/console-inline-panel`). Pedido del dueño del producto:
el botón de "cuatro cuadritos" debe abrir la consola COMO UN PANEL dentro de la
app (patrón `#screen-settings`), no como ventana aparte. Motivación de raíz: la
`WebviewWindow` aparte sin decoración sale en blanco en la máquina Windows del
dueño; Settings funciona porque es un `<div>` que ya vive en el HTML de la
ventana principal. Este documento registra las tres decisiones duras y su
evidencia. Es la referencia para la revisión de seguridad (VETO).

## 1. Cómo se cargan los assets premium → opción (a), con el gate MOVIDO al handler

El fuente de la consola es PRIVADO (repo `centinelo-premium`, paquete
`console-ui`) y se distribuye como recurso del instalador en
`premium-console-assets/` junto al ejecutable. PROHIBIDO copiarlo a este repo
público. Por lo tanto, sea cual sea la opción, el panel debe traer esos archivos
en runtime.

### Opción (b) literal — "servir por el MISMO origen (interceptar `/premium-console/...`)": NO es implementable con API pública

Verificado contra el fuente de `tauri` 2.11.5 (crates.io, en el registry local):

- El origen de la ventana principal (`tauri://localhost` en macOS/Linux,
  `http(s)://tauri.localhost` en Windows — `tauri::manager::mod` ~línea 338) lo
  sirve el protocolo de assets que **Tauri core registra él mismo** a través de
  `tauri-runtime-wry`. Su resolución pasa por `AppManager::get_asset`
  (`tauri/src/manager/mod.rs:384`), que solo lee el `AssetResolver`
  (frontendDist embebido). Es read-only: no hay hook público para añadir rutas.
- `App::register_uri_scheme_protocol` (`tauri/src/app.rs:2130`) solo registra
  esquemas NUEVOS. No puede interceptar ni reemplazar un esquema que core ya
  registró (`tauri`, `http://tauri.localhost`, `ipc`, `asset`).

Las variantes "mismo origen" que sí son implementables son en realidad (c):

### Opción (c)/(b') — leer en Rust e inyectar (blob): DEBILITA MÁS la CSP

Inyectar texto de `<script>` cuenta como inline script: bloqueado por
`script-src` sin `'unsafe-inline'`. El único camino es `blob:` URLs, o sea
`script-src 'self' blob:`. Trade-off real: **`blob:` autoriza a CUALQUIER JS
corriendo en la página** (un XSS puede mintar un blob y ejecutarlo) — amplifica
exactamente la clase de ataque que `script-src` sin `unsafe-inline` existe para
contener. `eval`/`unsafe-inline`: ni considerarlos.

### Opción (a) ELEGIDA — `script-src`/`style-src` + `premium-console:`, con gate de licencia en el handler

La CSP de la ventana principal queda:

```
default-src 'self';
script-src 'self' premium-console:;
style-src 'self' 'unsafe-inline' premium-console:;
img-src 'self' data:;
connect-src 'self' ipc: http://ipc.localhost
```

Argumento de seguridad (para el revisor con VETO):

1. **El añadido es un origen de confianza equivalente a `'self'`.** El esquema
   `premium-console://` lo sirve exclusivamente `console.rs::asset_protocol_handler`
   — un file server nuestro con guard de path-traversal (canonicalize +
   starts_with) que solo lee de `premium-console-assets/` junto al exe. Para
   inyectar código por esa vía, un atacante necesita ESCRITURA de archivos
   locales en ese directorio — con la que ya podría reemplazar el ejecutable o
   los assets del propio frontendDist. No introduce ninguna fuente remota.
2. **Desde este cambio, el handler comprueba la licencia** (`is_unlocked` via
   `UriSchemeContext::app_handle()`, disponible en `tauri/src/app.rs:2476`) y
   responde 404 si `blf_console` no está licenciado. Es MÁS fuerte que el status
   quo: hoy el handler sirve archivos sin comprobar nada (confiaba en que "la
   ventana nunca se crea sin licencia"; un main-window con devtools podía pedir
   el esquema directamente, aunque CORS le impedía leer la respuesta).
3. **No se toca `connect-src`** — la lección del incidente CSP 2.0.3 (branch
   `fix/csp-allow-ipc`): IPC sigue exactamente igual.
4. El CSS privado no tiene `url()`/`@font-face` (verificado) → no hace falta
   tocar `font-src`/`img-src`. Los tokens públicos y privados son los mismos
   valores Vigilia verbatim (ver §4).

Por qué NO era deseable "matar el esquema" a cualquier costo: el riesgo real de
los últimos dos días no era el scheme handler (un file server tonto y
logueado), sino (i) la ventana APARTE sin decoración (operador atrapado,
watchdog, cierre-por-fallo) y (ii) el arranque en un documento huérfano. Con el
panel, TODO eso desaparece: si los scripts fallan, el panel muestra un banner
de error dentro de una ventana que siempre tiene su propio chrome y su propio
cierra; no hace falta watchdog ni `mark_ready` ni cierre-por-fallo. El scheme
queda reducido a lo que (b) habría pedido: un path de servido de assets,
ahora con gate.

## 2. El gate no se toca — se reubica y se refuerza

Superficie de gating, de afuera hacia adentro:

1. **Botón ausente** (`#btn-console` con `hidden` hasta que
   `premium_capability_status(blf_console)` + `blf_enabled` lo muestran) — sin
   cambio.
2. **Tray ausente** (`tray.rs` no agrega el ítem sin `is_unlocked`) — sin
   cambio.
3. **`open_console` re-comprueba la licencia** antes de hacer nada (igual que
   `open_or_focus` hoy: un webview con devtools puede invocar el comando
   directamente). Sin licencia → `Err("premium console is not licensed")`.
4. **NUEVO — el handler 404 para sin-licencia.** Un usuario que manipule el
   frontend (devtools, mostrar el div a mano) obtiene: panel vacío + 404s en
   todos los assets. El código de la consola nunca llega al DOM.
5. Los verbos sidecar que la consola usa (`sidecar_dial`, `blf_subscribe`, …)
   ya eran invocables desde la ventana principal ANTES de este cambio (el grid
   de favorites los usa) — la superficie de comandos no cambia en nada.

## 3. Permisos IPC — cero cambios

- `capabilities/console.json` (ventana "console") se MANTIENE: la ventana
  aparte sigue existiendo detrás de la bandera de transición (§5) y la necesita.
- `capabilities/default.json` (ventana "main") NO se amplía. El panel necesita:
  - eventos (`listen`) → ya cubierto por `core:default`;
  - invoke de comandos propios del app → los comandos de la app no pasan por ACL;
  - minimizar/arrastrar (título del panel) → `core:window:allow-minimize` y
    `core:window:allow-start-dragging` ya están en default.json;
  - "cerrar" el panel → puro DOM (`hidden = true`), sin API.
- El redimensionado de la ventana al abrir/cerrar el panel se hace EN RUST
  (`open_console` / `console_panel_closed`), precisamente para no tener que
  añadir `core:window:allow-set-size` a la ventana principal.

## 4. Coexistencia de tokens (público + privado en el mismo documento)

El panel inyecta `<link href="premium-console://localhost/tokens.css">` en el
documento principal, que YA carga `css/tokens.css`. Ambos archivos son el mismo
Vigilia verbatim (diff verificado: solo difieren comentarios de procedencia y
2 tokens extra del lado público, `--voice-*`, que el privado no redefine). El
segundo `:root` re-declara los mismos valores → cero cambio visual. La consola
respeta así `data-theme` del documento (mecanismo documentado por el propio
paquete: "whoever hosts this page sets data-theme on the document root").
Nota de dirección de arte: el CSS privado usa ámbar para focus-ring/skip-link
del paquete; eso es diseño del paquete privado (no se modifica código privado
aquí); en el lado público del panel no se introduce ámbar nuevo.

## 5. Ventana aparte: bandera de transición, no borrado

`open_or_focus` (ventana + watchdog + INDEX_HTML embebido) se CONSERVA integro,
alcanzable únicamente con `CENTINELO_CONSOLE_SEPARATE_WINDOW=1` (default: panel).
Se elimina cuando el panel se valide en el Windows del dueño. Mientras tanto,
tray y `open_console` enrutan por `console::open()`, que decide según la bandera.

## 6. Tamaño de ventana mientras el panel está abierto

El paquete console-ui declara `min-width:760px` (`.cent-console.win`) y su layout
es `rail(56) + main(grid ≥150px/tile) + side(288px)`. La ventana principal por
default es 380px: el panel la rompería. Decisión: al abrir el panel, Rust agranda
la ventana principal a `max(actual, 900×640)` y sube el mínimo a `760×520`
(el piso del propio paquete) mientras esté abierto; al cerrar, restaura el
tamaño y el mínimo previos. La consola NO es una ventana aparte: es el mismo
panel inset:0 estilo Settings, en la misma ventana, con el mismo header con
flecha de volver. Si el dueño prefiere que la app NI se agrande, el punto de
ajuste es una sola constante (`PANEL_TARGET_SIZE`).

## 7. Teclado

Settings se cierra con su botón back (no con Esc). El panel copia eso, y NO
liga Esc a cerrar el panel a propósito: la consola tiene contrato de teclado
propio ("Esc cancel" — cancelar transferencia / modos internos, documentado en
su sidetip); un Esc global del shell se lo robaría. El header con back es el
"volver como vuelve Settings".
