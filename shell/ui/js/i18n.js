// Centinelo Phone shell — i18n (English / Brazilian Portuguese / Spanish).
//
// Vanilla, no framework — a flat key -> string table per locale plus a
// tiny `t(key, vars)` resolver, matching this project's own "no bundler,
// no frontend framework" rule (see shell/README.md). Both app.js and
// transcript-panel.js import `t` from here; only app.js ever touches the
// DOM directly (locale selection, applying it to [data-i18n*] elements).
//
// Register split (per creative-vigilia's "voz técnica grabada" direction,
// premium/design/TOKENS.md §1.4 and this sprint's task brief): raw
// protocol/technical tokens that read the same in every language — "EXT",
// the transport codes "WSS"/"UDP"/"WSS→UDP" in the transport cards, TLS/
// crypto strings — stay hardcoded in app.js/index.html, NOT in this
// dictionary. Everything else a human reads as a sentence, label, or
// status word (including short mono-styled status words like "Live" or
// "Connecting" — the mono/uppercase look is a CSS treatment, not a
// language) is translated here.
//
// Adding a language: add its 4-letter/BCP47 tag to SUPPORTED_LOCALES and
// a 4th column to every ENTRIES row (falls back to the `en` column for
// any row you haven't gotten to yet — see `t()`).

export const SUPPORTED_LOCALES = ["en", "pt-BR", "es"];

// [key, en, "pt-BR", es]
const ENTRIES = [
  // -- titlebar --------------------------------------------------------
  ["titlebar.brandAria", "Centinelo — watching, line healthy", "Centinelo — vigiando, linha saudável", "Centinelo — vigilando, línea saludable"],
  ["titlebar.consoleAria", "Console", "Console", "Consola"],
  ["titlebar.consoleTitle", "Open the receptionist console", "Abrir o console da recepção", "Abrir la consola de recepción"],
  ["titlebar.transcriptAria", "Transcript", "Transcrição", "Transcripción"],
  ["titlebar.transcriptTitle", "View this call's transcript", "Ver a transcrição desta chamada", "Ver la transcripción de esta llamada"],
  ["titlebar.settingsAria", "Settings", "Configurações", "Configuración"],
  ["titlebar.minimizeAria", "Minimize", "Minimizar", "Minimizar"],
  ["titlebar.closeAria", "Close", "Fechar", "Cerrar"],

  // -- setup / onboarding ------------------------------------------------
  ["setup.heading", "Connect your phone system", "Conecte seu sistema telefônico", "Conecta tu sistema telefónico"],
  [
    "setup.body",
    "Paste the provisioning link your installer sent you, or set it up by hand in Settings.",
    "Cole o link de provisionamento que o instalador enviou, ou configure manualmente em Configurações.",
    "Pega el enlace de aprovisionamiento que te envió tu instalador, o configúralo a mano en Configuración.",
  ],
  ["setup.inputAria", "Provisioning link", "Link de provisionamento", "Enlace de aprovisionamiento"],
  ["setup.inputPlaceholder", "Paste your provisioning link", "Cole seu link de provisionamento", "Pega tu enlace de aprovisionamiento"],
  ["setup.connect", "Connect", "Conectar", "Conectar"],
  [
    "setup.hint",
    "A provisioning link contains your account. Treat it like a password.",
    "Um link de provisionamento contém sua conta. Trate-o como uma senha.",
    "Un enlace de aprovisionamiento contiene tu cuenta. Trátalo como una contraseña.",
  ],
  ["setup.manualSetup", "Set up by hand in Settings", "Configurar manualmente em Configurações", "Configurar a mano en Configuración"],

  // -- main / idle window ------------------------------------------------
  ["main.accountAria", "Your account", "Sua conta", "Tu cuenta"],
  ["main.regPillTitle", "Registration status", "Status do registro", "Estado del registro"],
  ["main.dialNumAria", "Number to dial", "Número a discar", "Número a marcar"],
  ["main.backspaceAria", "Delete last digit", "Apagar último dígito", "Borrar el último dígito"],
  ["main.dialpadAria", "Dial pad", "Teclado", "Teclado"],
  ["main.callAria", "Call", "Ligar", "Llamar"],
  ["main.favoritesHeading", "Favorites", "Favoritos", "Favoritos"],
  ["main.favoritesAria", "Favorites", "Favoritos", "Favoritos"],
  ["main.recentHeading", "Recent", "Recentes", "Recientes"],
  ["main.recentAria", "Recent calls", "Chamadas recentes", "Llamadas recientes"],
  [
    "main.recentsEmpty",
    "Calls you make and take will show up here.",
    "As chamadas que você fizer e receber vão aparecer aqui.",
    "Las llamadas que hagas y recibas aparecerán aquí.",
  ],
  ["main.callMissed", "Missed", "Perdida", "Perdida"],
  ["main.callOutgoing", "Outgoing", "Efetuada", "Saliente"],
  ["main.callMissedCall", "Missed call", "Chamada perdida", "Llamada perdida"],
  ["main.callIncoming", "Incoming", "Recebida", "Entrante"],
  ["main.yesterday", "Yesterday", "Ontem", "Ayer"],
  ["main.dialPlaceholder", "Dial a number", "Digite um número", "Marca un número"],

  // -- favorites grid ------------------------------------------------------
  ["favorites.empty", "Empty", "Vazio", "Vacío"],
  ["favorites.notSetUp", "Not set up", "Não configurado", "Sin configurar"],
  ["favorites.notTrackedYet", "Not tracked yet", "Ainda não monitorado", "Aún no monitoreado"],
  ["favorites.available", "Available", "Disponível", "Disponible"],
  ["favorites.ringing", "Ringing", "Chamando", "Sonando"],
  ["favorites.onCall", "On a call", "Em chamada", "En llamada"],
  ["favorites.offline", "Offline", "Offline", "Sin conexión"],
  ["favorites.extFallback", "Ext {ext}", "Ramal {ext}", "Ext. {ext}"],
  ["favorites.callingName", "Calling {name}.", "Ligando para {name}.", "Llamando a {name}."],

  // -- call overlay ------------------------------------------------------
  ["call.ringingEllipsis", "Ringing…", "Chamando…", "Sonando…"],
  ["call.callingEllipsis", "Calling…", "Ligando…", "Llamando…"],
  ["call.incomingCallLabel", "Incoming call", "Chamada recebida", "Llamada entrante"],
  ["call.mainLine", "Main line", "Linha principal", "Línea principal"],
  ["call.transcribeThisCall", "Transcribe this call", "Transcrever esta chamada", "Transcribir esta llamada"],
  ["call.encryptedCall", "Encrypted call", "Chamada criptografada", "Llamada cifrada"],
  ["call.decline", "Decline", "Recusar", "Rechazar"],
  ["call.answer", "Answer", "Atender", "Contestar"],
  ["call.endCall", "End call", "Encerrar chamada", "Finalizar llamada"],
  [
    "call.cantCallBusy",
    "Can't call {number} — you're already on a call.",
    "Não é possível ligar para {number} — você já está em uma chamada.",
    "No se puede llamar a {number} — ya tienes una llamada en curso.",
  ],
  [
    "call.cantDialBusy",
    "Can't dial {number} — you're already on a call.",
    "Não é possível discar {number} — você já está em uma chamada.",
    "No se puede marcar {number} — ya tienes una llamada en curso.",
  ],
  [
    "call.dialingFrom",
    "Dialing {number} from {source}.",
    "Ligando para {number} a partir de {source}.",
    "Llamando a {number} desde {source}.",
  ],
  [
    "call.addAccountFirst",
    "Add your phone system in Settings first.",
    "Adicione seu sistema telefônico em Configurações primeiro.",
    "Agrega tu sistema telefónico en Configuración primero.",
  ],

  // -- registration pill ------------------------------------------------
  ["regPill.registeredTitle", "Registered — {transport} transport", "Registrado — transporte {transport}", "Registrado — transporte {transport}"],
  [
    "regPill.failedTitle",
    "Can't reach your phone system — retrying automatically.",
    "Não é possível alcançar seu sistema telefônico — tentando novamente de forma automática.",
    "No se puede contactar tu sistema telefónico — reintentando automáticamente.",
  ],
  ["regPill.notRegisteredTitle", "Not registered yet.", "Ainda não registrado.", "Aún no registrado."],
  ["regPill.connecting", "Connecting", "Conectando", "Conectando"],
  ["regPill.retrying", "Retrying", "Tentando novamente", "Reintentando"],
  ["regPill.offline", "Offline", "Offline", "Sin conexión"],

  // -- titlebar dynamic state ------------------------------------------------
  ["titlebarState.notSetUp", "Not set up", "Não configurado", "Sin configurar"],
  ["titlebarState.onCallWith", "On a call — {who}", "Em chamada — {who}", "En llamada — {who}"],
  ["titlebarState.ringingWith", "Ringing — {who}", "Chamando — {who}", "Sonando — {who}"],
  ["titlebarState.incomingWith", "Incoming — {who}", "Recebendo — {who}", "Entrante — {who}"],
  ["titlebarState.callingWith", "Calling {who}…", "Ligando para {who}…", "Llamando a {who}…"],
  ["titlebarState.starting", "Starting…", "Iniciando…", "Iniciando…"],
  [
    "titlebarState.reconnecting",
    "Reconnecting the phone engine… ({attempt}/{max})",
    "Reconectando o motor do telefone… ({attempt}/{max})",
    "Reconectando el motor del teléfono… ({attempt}/{max})",
  ],
  ["titlebarState.stopped", "Phone engine stopped", "Motor do telefone parado", "Motor del teléfono detenido"],
  ["titlebarState.crashed", "Phone engine crashed — see Settings", "O motor do telefone travou — veja Configurações", "El motor del teléfono falló — revisa Configuración"],
  ["titlebarState.connecting", "Connecting…", "Conectando…", "Conectando…"],
  [
    "titlebarState.cantReachRetrying",
    "Can't reach your phone system — retrying",
    "Não é possível alcançar seu sistema telefônico — tentando novamente",
    "No se puede contactar tu sistema telefónico — reintentando",
  ],
  ["titlebarState.ready", "Ready", "Pronto", "Listo"],

  // -- settings screen ------------------------------------------------
  ["settings.backAria", "Back", "Voltar", "Atrás"],
  ["settings.title", "Settings", "Configurações", "Configuración"],
  ["settings.general", "General", "Geral", "General"],
  ["settings.displayNameLabel", "Display name", "Nome de exibição", "Nombre para mostrar"],
  ["settings.displayNamePlaceholder", "Front desk", "Recepção", "Recepción"],
  ["settings.themeAria", "Theme", "Tema", "Tema"],
  ["settings.themeAuto", "Auto", "Automático", "Automático"],
  ["settings.themeLight", "Light", "Claro", "Claro"],
  ["settings.themeDark", "Dark", "Escuro", "Oscuro"],
  ["settings.languageAria", "Language", "Idioma", "Idioma"],
  ["settings.langAuto", "Auto", "Automático", "Automático"],
  ["settings.langAutoTitle", "Match this computer's language", "Usar o idioma deste computador", "Usar el idioma de este equipo"],
  ["settings.langEn", "EN", "EN", "EN"],
  ["settings.langEnTitle", "English", "Inglês", "Inglés"],
  ["settings.langPtBr", "PT-BR", "PT-BR", "PT-BR"],
  ["settings.langPtBrTitle", "Português (Brasil)", "Português (Brasil)", "Portugués (Brasil)"],
  ["settings.langEs", "ES", "ES", "ES"],
  ["settings.langEsTitle", "Español", "Espanhol", "Español"],
  ["settings.phoneSystem", "Phone system", "Sistema telefônico", "Sistema telefónico"],
  ["settings.serverHostLabel", "Server host", "Servidor", "Servidor"],
  ["settings.serverHostPlaceholder", "pbx.example.com", "pbx.exemplo.com", "pbx.ejemplo.com"],
  ["settings.extensionLabel", "Extension", "Ramal", "Extensión"],
  ["settings.extensionPlaceholder", "1000", "1000", "1000"],
  ["settings.passwordLabel", "Password", "Senha", "Contraseña"],
  ["settings.passwordPlaceholder", "Unchanged", "Sem alterações", "Sin cambios"],
  [
    "settings.secretCurrentlySet",
    "Currently set — leave blank to keep it unchanged.",
    "Já definida — deixe em branco para mantê-la sem alterações.",
    "Ya está definida — déjala en blanco para no cambiarla.",
  ],
  ["settings.secretNotSet", "Not set yet.", "Ainda não definida.", "Aún no definida."],
  ["settings.favoritesHeading", "Favorites", "Favoritos", "Favoritos"],
  [
    "settings.favoritesHint",
    "Up to 4 extensions with live status on the main window. Calling one always asks for confirmation first.",
    "Até 4 ramais com status ao vivo na janela principal. Ligar para um deles sempre pede confirmação antes.",
    "Hasta 4 extensiones con estado en vivo en la ventana principal. Llamar a una siempre pide confirmación primero.",
  ],
  ["settings.favLabelLabel", "Label", "Rótulo", "Etiqueta"],
  ["settings.favLabelPlaceholder", "Front desk", "Recepção", "Recepción"],
  ["settings.favExtLabel", "Extension", "Ramal", "Extensión"],
  ["settings.favExtPlaceholder", "Empty", "Vazio", "Vacío"],
  ["settings.transportHeading", "How calls travel", "Como as chamadas trafegam", "Cómo viajan las llamadas"],
  ["settings.transportAutoName", "Auto", "Automático", "Automático"],
  [
    "settings.transportAutoDesc",
    "Centinelo tries the secure route first and falls back automatically if it can't register.",
    "O Centinelo tenta primeiro a rota segura e recua automaticamente se não conseguir se registrar.",
    "Centinelo intenta primero la ruta segura y retrocede automáticamente si no logra registrarse.",
  ],
  ["settings.transportClassicName", "Classic SIP", "SIP clássico", "SIP clásico"],
  [
    "settings.transportClassicDesc",
    "The traditional route — best when your phone system is on the same network.",
    "A rota tradicional — melhor quando seu sistema telefônico está na mesma rede.",
    "La ruta tradicional — mejor cuando tu sistema telefónico está en la misma red.",
  ],
  ["settings.transportWssName", "Secure web", "Web segura", "Web segura"],
  [
    "settings.transportWssDesc",
    "Calls travel inside an encrypted web connection — best across the internet and strict firewalls.",
    "As chamadas trafegam dentro de uma conexão web criptografada — melhor pela internet e firewalls restritos.",
    "Las llamadas viajan dentro de una conexión web cifrada — mejor a través de internet y firewalls estrictos.",
  ],
  ["settings.bridgeHeading", "Click-to-call", "Clique para ligar", "Clic para llamar"],
  [
    "settings.bridgeHint",
    "Dial numbers you click on the web with the Centinelo browser extension. Runs on this computer only.",
    "Disque números que você clica na web com a extensão do navegador Centinelo. Funciona apenas neste computador.",
    "Marca números en los que haces clic en la web con la extensión de navegador de Centinelo. Funciona solo en este equipo.",
  ],
  ["settings.bridgeAddressLabel", "Bridge address", "Endereço da ponte", "Dirección del puente"],
  ["settings.bridgeTokenLabel", "Pairing token", "Token de pareamento", "Token de emparejamiento"],
  ["settings.copy", "Copy", "Copiar", "Copiar"],
  ["settings.copied", "Copied.", "Copiado.", "Copiado."],
  ["settings.dialAutomaticallyLabel", "Dial automatically", "Discar automaticamente", "Marcar automáticamente"],
  ["settings.boolOff", "Off", "Desligado", "Desactivado"],
  ["settings.boolOn", "On", "Ligado", "Activado"],
  [
    "settings.dialAutomaticallyHint",
    'When off, every click-to-call and centinelo:// or tel: link asks "Call this number?" first.',
    'Quando desligado, todo clique para ligar e link centinelo:// ou tel: pergunta "Ligar para este número?" antes.',
    'Cuando está desactivado, cada clic para llamar y enlace centinelo:// o tel: pregunta "¿Llamar a este número?" primero.',
  ],
  ["settings.telHandlerLabel", "Answer tel: links", "Responder links tel:", "Responder enlaces tel:"],
  [
    "settings.telHandlerHint",
    "Lets Centinelo offer to dial tel: links from other apps.",
    "Permite que o Centinelo ofereça discar links tel: de outros aplicativos.",
    "Permite que Centinelo ofrezca marcar enlaces tel: de otras aplicaciones.",
  ],
  // -- settings / transcription (Plate 08) --------------------------------
  ["settings.transcriptionHeading", "Transcription", "Transcrição", "Transcripción"],
  [
    "settings.transcriptionTrustLine",
    "Calls are transcribed on this computer — nothing is sent anywhere.",
    "As chamadas são transcritas neste computador — nada é enviado a lugar nenhum.",
    "Las llamadas se transcriben en este equipo — no se envía nada a ningún lado.",
  ],
  ["settings.transcriptionModeLabel", "When to transcribe", "Quando transcrever", "Cuándo transcribir"],
  ["settings.transcriptionModeOffName", "Off", "Desligado", "Desactivado"],
  ["settings.transcriptionModeOffDesc", "Calls are not transcribed.", "As chamadas não são transcritas.", "Las llamadas no se transcriben."],
  ["settings.transcriptionModeLiveName", "Live", "Ao vivo", "En vivo"],
  [
    "settings.transcriptionModeLiveDesc",
    "A panel follows the conversation during the call, a few seconds behind.",
    "Um painel acompanha a conversa durante a chamada, com poucos segundos de atraso.",
    "Un panel sigue la conversación durante la llamada, unos segundos por detrás.",
  ],
  ["settings.transcriptionModePostCallName", "After the call", "Depois da chamada", "Después de la llamada"],
  [
    "settings.transcriptionModePostCallDesc",
    "The transcript is written once the call ends. Easier on slower computers.",
    "A transcrição é gravada assim que a chamada termina. Mais leve em computadores mais lentos.",
    "La transcripción se escribe cuando termina la llamada. Más ligero en equipos más lentos.",
  ],
  ["settings.transcriptionActivationLabel", "Which calls", "Quais chamadas", "Qué llamadas"],
  ["settings.transcriptionActivationAllName", "Every call", "Toda chamada", "Cada llamada"],
  [
    "settings.transcriptionActivationAllDesc",
    "Each answered call is transcribed automatically.",
    "Cada chamada atendida é transcrita automaticamente.",
    "Cada llamada atendida se transcribe automáticamente.",
  ],
  ["settings.transcriptionActivationManualName", "Only when turned on", "Somente quando ativado", "Solo cuando se activa"],
  [
    "settings.transcriptionActivationManualDesc",
    'Nothing is transcribed unless someone starts it for that call — a "Transcribe this call" button joins the call controls.',
    'Nada é transcrito a menos que alguém inicie para aquela chamada — um botão "Transcrever esta chamada" aparece nos controles da chamada.',
    'No se transcribe nada a menos que alguien lo inicie para esa llamada — un botón "Transcribir esta llamada" se suma a los controles de la llamada.',
  ],
  ["settings.transcriptionStorageDirLabel", "Storage folder", "Pasta de armazenamento", "Carpeta de almacenamiento"],
  ["settings.transcriptionStorageDirPlaceholder", "//archive/front-desk/calls", "//arquivo/recepcao/chamadas", "//archivo/recepcion/llamadas"],
  [
    "settings.transcriptionStorageDirHint",
    "A local path or a mounted network folder. Point it at this computer and nothing ever leaves it.",
    "Um caminho local ou uma pasta de rede montada. Aponte para este computador e nada sai dele.",
    "Una ruta local o una carpeta de red montada. Apúntala a este equipo y nada sale de él.",
  ],
  ["settings.transcriptionKeepAudioLabel", "Keep the audio recordings", "Manter as gravações de áudio", "Conservar las grabaciones de audio"],
  [
    "settings.transcriptionKeepAudioHint",
    "Audio files stay next to their transcripts. Off deletes the audio once its transcript is written.",
    "Os arquivos de áudio ficam junto de suas transcrições. Desligado apaga o áudio assim que a transcrição é gravada.",
    "Los archivos de audio quedan junto a sus transcripciones. Desactivado borra el audio en cuanto se escribe la transcripción.",
  ],
  ["settings.transcriptionViewOnlyLabel", "View only", "Somente visualizar", "Solo lectura"],
  [
    "settings.transcriptionViewOnlyHint",
    "This computer reads transcripts from the folder but never writes new ones — for a manager's desk.",
    "Este computador lê as transcrições da pasta mas nunca grava novas — para a mesa de um gerente.",
    "Este equipo lee las transcripciones de la carpeta pero nunca escribe nuevas — para el escritorio de un gerente.",
  ],
  ["settings.transcriptionModelLabel", "The model — runs on this computer", "O modelo — roda neste computador", "El modelo — funciona en este equipo"],
  ["settings.transcriptionModelAccurateName", "Accurate", "Preciso", "Preciso"],
  [
    "settings.transcriptionModelAccurateDesc",
    "The best transcripts. Uses about 1 GB of memory while a call is being written.",
    "As melhores transcrições. Usa cerca de 1 GB de memória enquanto uma chamada está sendo gravada.",
    "Las mejores transcripciones. Usa cerca de 1 GB de memoria mientras se escribe una llamada.",
  ],
  ["settings.transcriptionModelLightName", "Light", "Leve", "Liviano"],
  [
    "settings.transcriptionModelLightDesc",
    "Good transcripts on modest computers — about half the memory.",
    "Boas transcrições em computadores mais modestos — cerca de metade da memória.",
    "Buenas transcripciones en equipos modestos — cerca de la mitad de la memoria.",
  ],
  ["settings.transcriptionModelInstalled", "Installed", "Instalado", "Instalado"],
  ["settings.transcriptionModelDownload", "Download", "Baixar", "Descargar"],
  ["settings.transcriptionModelDownloading", "Downloading…", "Baixando…", "Descargando…"],
  [
    "settings.transcriptionModelDownloadStalled",
    "The download stopped responding — try again.",
    "O download parou de responder — tente novamente.",
    "La descarga dejó de responder — intenta de nuevo.",
  ],
  ["settings.transcriptionModelCheckFailed", "Couldn't check", "Não foi possível verificar", "No se pudo verificar"],
  ["settings.transcriptionModelRetry", "Retry", "Tentar novamente", "Reintentar"],
  ["settings.transcriptionLanguageLabel", "Language", "Idioma", "Idioma"],
  ["settings.transcriptionLangAuto", "Auto", "Automático", "Automático"],
  ["settings.transcriptionLangEn", "EN", "EN", "EN"],
  ["settings.transcriptionLangEs", "ES", "ES", "ES"],
  ["settings.transcriptionLangPt", "PT", "PT", "PT"],
  [
    "settings.transcriptionLanguageHint",
    "Auto-detect picks the language per call.",
    "A detecção automática escolhe o idioma a cada chamada.",
    "La detección automática elige el idioma en cada llamada.",
  ],
  [
    "settings.transcriptionPrivacyNote",
    "Everything above happens on this machine. Audio and transcripts are written only to the folder you chose.",
    "Tudo acima acontece nesta máquina. Áudio e transcrições são gravados apenas na pasta que você escolheu.",
    "Todo lo anterior ocurre en este equipo. El audio y las transcripciones se escriben solo en la carpeta que elegiste.",
  ],

  // ---- remote STT (P6, SPEC-2026-07-17-remote-stt-design.md §7) --------
  // Same settings-card language as the rest of Settings → Transcription
  // above, kept as its own card + its own trust line (settings.
  // remoteSttTrustLine) rather than folded into transcriptionTrustLine's
  // blanket "nothing is sent anywhere" claim, which stops being true the
  // moment stt_mode is switched to Remote - see index.html's
  // #remote-stt-card comment for why this card sits AFTER
  // #transcription-section's own privacy note instead of before it.
  ["settings.sttModeLabel", "Speech-to-text engine", "Motor de transcrição", "Motor de transcripción"],
  ["settings.sttModeLocal", "Local", "Local", "Local"],
  ["settings.sttModeRemote", "Remote", "Remoto", "Remoto"],
  [
    "settings.sttModeHint",
    "Local runs entirely on this computer, as above. Remote sends each call's audio to the server below to be transcribed there instead.",
    "Local roda inteiramente neste computador, como acima. Remoto envia o áudio de cada chamada para o servidor abaixo, que faz a transcrição.",
    "Local funciona por completo en este equipo, como arriba. Remoto envía el audio de cada llamada al servidor de abajo para que la transcriba.",
  ],
  [
    "settings.remoteSttTrustLine",
    "Call audio leaves this computer only while Remote is selected, and only to the server below.",
    "O áudio das chamadas só sai deste computador enquanto Remoto estiver selecionado, e só para o servidor abaixo.",
    "El audio de las llamadas solo sale de este equipo mientras Remoto está seleccionado, y solo hacia el servidor de abajo.",
  ],
  ["settings.remoteSttBackendLabel", "Remote backend", "Backend remoto", "Backend remoto"],
  ["settings.remoteSttBackendCentinelo", "Centinelo", "Centinelo", "Centinelo"],
  ["settings.remoteSttBackendOpenaiCompat", "OpenAI-compatible", "Compatível com OpenAI", "Compatible con OpenAI"],
  ["settings.remoteSttUrlLabel", "Server URL", "URL do servidor", "URL del servidor"],
  ["settings.remoteSttUrlPlaceholder", "https://", "https://", "https://"],
  [
    "settings.remoteSttUrlHint",
    "https:// required, except http://127.0.0.1 or http://localhost for local testing.",
    "https:// é obrigatório, exceto http://127.0.0.1 ou http://localhost para testes locais.",
    "Se requiere https://, salvo http://127.0.0.1 o http://localhost para pruebas locales.",
  ],
  ["settings.remoteSttKeyLabel", "API key", "Chave de API", "Clave de API"],
  [
    "settings.remoteSttKeyHint",
    "Optional. Stored on this computer, never shown here again — retype it before saving if you restart the app, or leaving this blank on Save clears it.",
    "Opcional. Fica guardada neste computador e nunca é exibida aqui de novo — digite-a novamente antes de salvar se reiniciar o app, ou deixar em branco ao salvar a apaga.",
    "Opcional. Se guarda en este equipo y nunca se vuelve a mostrar aquí — vuelve a escribirla antes de guardar si reinicias la app, o dejarla en blanco al guardar la borra.",
  ],
  ["settings.remoteSttModelLabel", "Model", "Modelo", "Modelo"],
  ["settings.remoteSttModelPlaceholder", "whisper-large-v3", "whisper-large-v3", "whisper-large-v3"],
  [
    "settings.remoteSttModelHint",
    "Only used by the OpenAI-compatible backend.",
    "Usado apenas pelo backend compatível com OpenAI.",
    "Solo se usa con el backend compatible con OpenAI.",
  ],
  ["settings.remoteSttTestConnection", "Test connection", "Testar conexão", "Probar conexión"],
  ["settings.remoteSttTesting", "Testing…", "Testando…", "Probando…"],
  ["settings.remoteSttProbe.ok", "Connection successful.", "Conexão bem-sucedida.", "Conexión exitosa."],
  [
    "settings.remoteSttProbe.bad_url",
    "That doesn't look like a valid server URL.",
    "Isso não parece uma URL de servidor válida.",
    "Esa URL del servidor no es válida.",
  ],
  [
    "settings.remoteSttProbe.http_error",
    "The server answered but reported an error.",
    "O servidor respondeu, mas com um erro.",
    "El servidor respondió, pero con un error.",
  ],
  [
    "settings.remoteSttProbe.auth_required",
    "Reachable, but the API key was rejected.",
    "Alcançável, mas a chave de API foi rejeitada.",
    "Se pudo alcanzar, pero se rechazó la clave de API.",
  ],
  [
    "settings.remoteSttProbe.network",
    "Couldn't reach that server.",
    "Não foi possível contatar esse servidor.",
    "No se pudo contactar ese servidor.",
  ],
  ["settings.remoteSttProbe.locked", "Unlock Settings first.", "Desbloqueie Configurações primeiro.", "Desbloquea Configuración primero."],

  ["settings.advancedHeading", "Advanced", "Avançado", "Avanzado"],
  ["settings.corePathLabel", "Core engine path", "Caminho do motor principal", "Ruta del motor principal"],
  ["settings.corePathPlaceholder", "Auto-detected", "Detectado automaticamente", "Detectado automáticamente"],
  [
    "settings.corePathHint",
    "Leave blank to auto-detect the locally built engine.",
    "Deixe em branco para detectar automaticamente o motor compilado localmente.",
    "Déjalo en blanco para detectar automáticamente el motor compilado localmente.",
  ],
  ["settings.restartEngine", "Restart engine", "Reiniciar motor", "Reiniciar motor"],
  ["settings.restarting", "Restarting the phone engine…", "Reiniciando o motor do telefone…", "Reiniciando el motor del teléfono…"],

  // ---- license activation (P3 of the activation-server plan) ----------
  // Backend errors cross the Tauri command boundary as short codes (see
  // activation.rs's own doc, "Error codes, not prose") - the
  // "activation.error.*" keys below are what the codes actually render
  // as, in all three of this product's real languages, same as every
  // other user-facing string on this screen.
  ["settings.licenseHeading", "License", "Licença", "Licencia"],
  ["settings.licenseSerialLabel", "Serial", "Serial", "Serial"],
  ["settings.licenseSerialPlaceholder", "CENT1-…", "CENT1-…", "CENT1-…"],
  ["settings.licenseServerUrlLabel", "Activation server", "Servidor de ativação", "Servidor de activación"],
  ["settings.licenseServerUrlPlaceholder", "https://", "https://", "https://"],
  ["settings.licenseActivate", "Activate", "Ativar", "Activar"],
  ["settings.licenseActivating", "Activating…", "Ativando…", "Activando…"],
  ["settings.licenseSerialRequired", "Enter a serial first.", "Digite um serial primeiro.", "Ingresa un serial primero."],
  [
    "settings.licenseActivatedStatus",
    "License saved for {customer}.",
    "Licença salva para {customer}.",
    "Licencia guardada para {customer}.",
  ],
  [
    "settings.licenseAlreadyPresentHint",
    "A license is already saved on this machine. Activating again replaces it.",
    "Uma licença já está salva neste computador. Ativar novamente a substitui.",
    "Ya hay una licencia guardada en este equipo. Activar de nuevo la reemplaza.",
  ],
  [
    "settings.licenseNotActivatedHint",
    "No license saved on this machine yet.",
    "Nenhuma licença salva neste computador ainda.",
    "Todavía no hay una licencia guardada en este equipo.",
  ],
  ["activation.error.bad_url", "That doesn't look like a valid activation server URL.", "Isso não parece uma URL válida do servidor de ativação.", "Esa URL del servidor de activación no es válida."],
  ["activation.error.invalid_serial", "Invalid serial.", "Serial inválido.", "Serial inválido."],
  ["activation.error.expired_serial", "Serial expired.", "Serial expirado.", "Serial expirado."],
  ["activation.error.bad_fingerprint", "Couldn't identify this machine.", "Não foi possível identificar este computador.", "No se pudo identificar este equipo."],
  ["activation.error.seats_exhausted", "No seats available on this serial.", "Sem vagas disponíveis neste serial.", "Sin asientos disponibles en este serial."],
  ["activation.error.serial_revoked", "Serial revoked.", "Serial revogado.", "Serial revocado."],
  ["activation.error.rate_limited", "Too many attempts — try again in a moment.", "Muitas tentativas — tente novamente em instantes.", "Demasiados intentos. Intenta de nuevo en un momento."],
  ["activation.error.network", "Couldn't reach the activation server.", "Não foi possível contatar o servidor de ativação.", "No se pudo contactar el servidor de activación."],
  ["activation.error.server_error", "The activation server returned an unexpected error.", "O servidor de ativação retornou um erro inesperado.", "El servidor de activación devolvió un error inesperado."],
  ["activation.error.local_verify_failed", "The license we received couldn't be verified.", "A licença recebida não pôde ser verificada.", "La licencia recibida no pudo verificarse."],
  ["activation.error.fingerprint_unavailable", "Couldn't identify this machine.", "Não foi possível identificar este computador.", "No se pudo identificar este equipo."],
  ["activation.error.io_error", "Couldn't save the license on this machine.", "Não foi possível salvar a licença neste computador.", "No se pudo guardar la licencia en este equipo."],

  ["settings.adminHeading", "Admin password", "Senha de administrador", "Contraseña de administrador"],
  [
    "settings.adminHint",
    "Protects account, transport and advanced settings from casual changes at the front desk.",
    "Protege as configurações de conta, transporte e avançadas contra alterações casuais na recepção.",
    "Protege la cuenta, el transporte y la configuración avanzada de cambios casuales en la recepción.",
  ],
  ["settings.adminNewLabel", "New admin password", "Nova senha de administrador", "Nueva contraseña de administrador"],
  ["settings.adminNewPlaceholder", "At least 8 characters", "Pelo menos 8 caracteres", "Al menos 8 caracteres"],
  ["settings.setPassword", "Set password", "Definir senha", "Establecer contraseña"],
  ["settings.passwordUpdated", "Password updated.", "Senha atualizada.", "Contraseña actualizada."],
  ["settings.useAtLeast8", "Use at least 8 characters.", "Use pelo menos 8 caracteres.", "Usa al menos 8 caracteres."],
  ["settings.aboutHeading", "About", "Sobre", "Acerca de"],
  [
    "settings.aboutBody",
    "Centinelo Phone 2.0 — shell build. Runs on this computer; nothing phones home.",
    "Centinelo Phone 2.0 — build do shell. Roda neste computador; nada é enviado para fora.",
    "Centinelo Phone 2.0 — build del shell. Funciona en este equipo; nada se envía afuera.",
  ],
  ["settings.updaterCheckOnStartupLabel", "Check for updates automatically", "Verificar atualizações automaticamente", "Buscar actualizaciones automáticamente"],
  ["settings.locked", "Settings are locked", "As configurações estão bloqueadas", "La configuración está bloqueada"],
  [
    "settings.lockedBody",
    "Enter the admin password to edit account, transport and advanced settings.",
    "Digite a senha de administrador para editar conta, transporte e configurações avançadas.",
    "Ingresa la contraseña de administrador para editar la cuenta, el transporte y la configuración avanzada.",
  ],
  ["settings.adminPasswordPlaceholder", "Admin password", "Senha de administrador", "Contraseña de administrador"],
  ["settings.unlock", "Unlock", "Desbloquear", "Desbloquear"],
  ["settings.notNow", "Not now", "Agora não", "Ahora no"],
  ["settings.incorrectPassword", "Incorrect password.", "Senha incorreta.", "Contraseña incorrecta."],
  ["settings.setAdminPassword", "Set an admin password", "Definir uma senha de administrador", "Establece una contraseña de administrador"],
  [
    "settings.setAdminPasswordBody",
    "This protects your phone system settings. You'll only be asked once per app launch.",
    "Isso protege as configurações do seu sistema telefônico. Você só será solicitado uma vez por sessão do aplicativo.",
    "Esto protege la configuración de tu sistema telefónico. Solo se te pedirá una vez por sesión de la aplicación.",
  ],
  ["settings.setPasswordContinue", "Set password & continue", "Definir senha e continuar", "Establecer contraseña y continuar"],
  ["settings.cancel", "Cancel", "Cancelar", "Cancelar"],
  ["settings.save", "Save", "Salvar", "Guardar"],
  ["settings.saving", "Saving…", "Salvando…", "Guardando…"],
  ["settings.savedReconnecting", "Saved — reconnecting…", "Salvo — reconectando…", "Guardado — reconectando…"],
  [
    "settings.savedAccountTranscriptionFailed",
    "Account saved; transcription NOT saved: {reason}",
    "Conta salva; transcrição NÃO salva: {reason}",
    "Cuenta guardada; transcripción NO guardada: {reason}",
  ],

  // -- auto-updater (roadmap debt fix) -----------------------------------
  // Settings > About status line (renderUpdaterAboutStatus, app.js) and the
  // non-intrusive main-window banner (renderUpdateBanner, ui/js/updater.js)
  // share this whole block - see updater.js's own header comment for why
  // only a subset of these phases ever reach the banner.
  ["updater.currentVersion", "Version {version}", "Versão {version}", "Versión {version}"],
  ["updater.checkButton", "Check for updates", "Verificar atualizações", "Buscar actualizaciones"],
  ["updater.checking", "Checking for updates…", "Verificando atualizações…", "Buscando actualizaciones…"],
  ["updater.upToDate", "You're up to date.", "Você está atualizado.", "Estás actualizado."],
  ["updater.aboutAvailable", "Update available — v{version}", "Atualização disponível — v{version}", "Actualización disponible — v{version}"],
  [
    "updater.aboutDownloading",
    "Downloading update… {pct}%",
    "Baixando atualização… {pct}%",
    "Descargando actualización… {pct}%",
  ],
  ["updater.aboutDownloadingIndeterminate", "Downloading update…", "Baixando atualização…", "Descargando actualización…"],
  ["updater.aboutReady", "Update ready — v{version}", "Atualização pronta — v{version}", "Actualización lista — v{version}"],
  ["updater.installing", "Installing update…", "Instalando atualização…", "Instalando actualización…"],
  ["updater.errorStatus", "Couldn't update: {message}", "Não foi possível atualizar: {message}", "No se pudo actualizar: {message}"],

  ["updater.bannerAvailableTitle", "Update available", "Atualização disponível", "Actualización disponible"],
  [
    "updater.bannerAvailableDetail",
    "Version {version} is ready to download.",
    "A versão {version} está pronta para baixar.",
    "La versión {version} está lista para descargar.",
  ],
  ["updater.bannerDownloadingTitle", "Downloading update…", "Baixando atualização…", "Descargando actualización…"],
  ["updater.bannerReadyTitle", "Update ready", "Atualização pronta", "Actualización lista"],
  [
    "updater.bannerReadyDetail",
    "Version {version} is ready to install.",
    "A versão {version} está pronta para instalar.",
    "La versión {version} está lista para instalar.",
  ],
  ["updater.bannerInstallingTitle", "Installing update…", "Instalando atualização…", "Instalando actualización…"],
  ["updater.bannerErrorTitle", "Update failed", "Falha na atualização", "Error al actualizar"],
  // install() itself succeeded - only the automatic restart afterward
  // failed. Deliberately NOT "failed"/"error" language - the update is
  // already safely on disk (see updater.js's "errorOrigin: restart" doc).
  ["updater.bannerRestartFailedTitle", "Update installed", "Atualização instalada", "Actualización instalada"],
  [
    "updater.bannerRestartFailedDetail",
    "Version {version} is installed — restart to finish.",
    "A versão {version} está instalada — reinicie para concluir.",
    "La versión {version} está instalada — reinicia para terminar.",
  ],
  [
    "updater.restartFailedStatus",
    "Update installed — v{version}. Restart to finish.",
    "Atualização instalada — v{version}. Reinicie para concluir.",
    "Actualización instalada — v{version}. Reinicia para terminar.",
  ],

  ["updater.download", "Download", "Baixar", "Descargar"],
  ["updater.restartToUpdate", "Restart to update", "Reiniciar para atualizar", "Reiniciar para actualizar"],
  ["updater.retry", "Retry", "Tentar novamente", "Reintentar"],
  ["updater.later", "Later", "Mais tarde", "Más tarde"],
  [
    "updater.finishCallFirstTitle",
    "Finish your call first.",
    "Termine sua chamada primeiro.",
    "Termina tu llamada primero.",
  ],

  // -- auto-provisioning ------------------------------------------------
  [
    "provisioning.transportAuto",
    "Auto — secure web, falls back to classic SIP",
    "Automático — web segura, recua para SIP clássico",
    "Automático — web segura, retrocede a SIP clásico",
  ],
  ["provisioning.transportWss", "Secure web (WSS)", "Web segura (WSS)", "Web segura (WSS)"],
  ["provisioning.transportClassic", "Classic SIP (UDP/TLS)", "SIP clássico (UDP/TLS)", "SIP clásico (UDP/TLS)"],
  ["provisioning.extensionOnly", "Extension {ext}", "Ramal {ext}", "Extensión {ext}"],
  ["provisioning.extensionNamed", "Extension {ext} — {name}", "Ramal {ext} — {name}", "Extensión {ext} — {name}"],
  ["provisioning.tlsPinIncluded", "{transport} · TLS pin included", "{transport} · pino TLS incluído", "{transport} · pin TLS incluido"],
  ["provisioning.connectQuestion", "Connect to this phone system?", "Conectar a este sistema telefônico?", "¿Conectar a este sistema telefónico?"],
  ["provisioning.willRegisterAs", "Centinelo will register as:", "O Centinelo vai se registrar como:", "Centinelo se registrará como:"],
  [
    "provisioning.passwordNeverShown",
    "The password is applied but never shown here.",
    "A senha é aplicada, mas nunca é exibida aqui.",
    "La contraseña se aplica pero nunca se muestra aquí.",
  ],
  ["provisioning.connect", "Connect", "Conectar", "Conectar"],
  ["provisioning.cancel", "Cancel", "Cancelar", "Cancelar"],
  [
    "provisioning.connectedRegistering",
    "Connected — registering with your phone system…",
    "Conectado — registrando com seu sistema telefônico…",
    "Conectado — registrando con tu sistema telefónico…",
  ],
  ["provisioning.linkError", "Provisioning link: {message}", "Link de provisionamento: {message}", "Enlace de aprovisionamiento: {message}"],

  // -- dial confirmation ------------------------------------------------
  ["dialConfirm.question", "Call this number?", "Ligar para este número?", "¿Llamar a este número?"],
  ["dialConfirm.call", "Call", "Ligar", "Llamar"],
  ["dialConfirm.notNow", "Not now", "Agora não", "Ahora no"],
  ["dialConfirm.fromBrowser", "From your browser.", "Do seu navegador.", "Desde tu navegador."],
  ["dialConfirm.fromSource", "From {source}.", "De {source}.", "Desde {source}."],

  // -- click-to-call source labels (mid-sentence, lowercase) ------------
  ["clickToCallSource.bridge", "your browser", "seu navegador", "tu navegador"],
  ["clickToCallSource.tel", "a tel: link", "um link tel:", "un enlace tel:"],
  ["clickToCallSource.centinelo", "a centinelo: link", "um link centinelo:", "un enlace centinelo:"],
  ["clickToCallSource.fallback", "outside the app", "fora do aplicativo", "fuera de la aplicación"],

  // -- transcript panel entry point / app.js-level state -----------------
  ["transcript.backAria", "Back", "Voltar", "Atrás"],
  ["transcript.defaultTitle", "Transcript", "Transcrição", "Transcripción"],
  ["transcript.live", "Live", "Ao vivo", "En vivo"],
  ["transcript.writing", "Writing…", "Escrevendo…", "Escribiendo…"],
  ["transcript.saved", "Saved", "Salvo", "Guardado"],
  ["transcript.couldntSave", "Couldn't save", "Não foi possível salvar", "No se pudo guardar"],
  ["transcript.copiedToClipboard", "Transcript copied.", "Transcrição copiada.", "Transcripción copiada."],
  ["transcript.readyFor", "Transcript ready — {who}.", "Transcrição pronta — {who}.", "Transcripción lista — {who}."],
  [
    "transcript.hiccup",
    "Transcription had a hiccup — it will keep trying.",
    "A transcrição teve um pequeno problema — vai continuar tentando.",
    "La transcripción tuvo un contratiempo — seguirá intentando.",
  ],
  ["transcript.callWord", "call", "chamada", "llamada"],
  ["sidecar.engineStopped", "The phone engine stopped working.", "O motor do telefone parou de funcionar.", "El motor del teléfono dejó de funcionar."],
  ["sidecar.somethingWrong", "Something went wrong.", "Algo deu errado.", "Algo salió mal."],

  // -- transcript-panel.js rendered content ------------------------------
  ["panel.liveBadge", "Live", "Ao vivo", "En vivo"],
  [
    "panel.trailNote",
    "Runs a few seconds behind the conversation — turns land whole, already attributed. No word-by-word churn.",
    "Fica alguns segundos atrás da conversa — as falas chegam inteiras, já atribuídas. Sem oscilação palavra por palavra.",
    "Va unos segundos por detrás de la conversación — los turnos llegan completos, ya atribuidos. Sin cambios palabra por palabra.",
  ],
  ["panel.writingHeading", "Writing the transcript", "Escrevendo a transcrição", "Escribiendo la transcripción"],
  [
    "panel.writingBody",
    "This can take a few minutes on this computer. You can keep taking calls.",
    "Isso pode levar alguns minutos neste computador. Você pode continuar atendendo chamadas.",
    "Esto puede tardar unos minutos en este equipo. Puedes seguir recibiendo llamadas.",
  ],
  ["panel.outgoingCall", "Outgoing call", "Chamada efetuada", "Llamada saliente"],
  ["panel.incomingCall", "Incoming call", "Chamada recebida", "Llamada entrante"],
  ["panel.lasted", "Lasted {duration}", "Durou {duration}", "Duró {duration}"],
  ["panel.savedChip", "Saved", "Salvo", "Guardado"],
  [
    "panel.channelsFailedYouOnly",
    "Part of this call wasn't transcribed — your audio couldn't be read. What follows is what was captured, not the full call.",
    "Parte desta chamada não foi transcrita — não foi possível ler o seu áudio. O que segue é o que foi captado, não a chamada completa.",
    "Parte de esta llamada no se transcribió — no se pudo leer tu audio. Lo que sigue es lo que se captó, no la llamada completa.",
  ],
  [
    "panel.channelsFailedCallerOnly",
    "Part of this call wasn't transcribed — the caller's audio couldn't be read. What follows is what was captured, not the full call.",
    "Parte desta chamada não foi transcrita — não foi possível ler o áudio do interlocutor. O que segue é o que foi captado, não a chamada completa.",
    "Parte de esta llamada no se transcribió — no se pudo leer el audio del interlocutor. Lo que sigue es lo que se captó, no la llamada completa.",
  ],
  [
    "panel.channelsFailedBoth",
    "Part of this call wasn't transcribed — neither side's audio could be read. What follows is what was captured, not the full call.",
    "Parte desta chamada não foi transcrita — não foi possível ler o áudio de nenhum dos dois lados. O que segue é o que foi captado, não a chamada completa.",
    "Parte de esta llamada no se transcribió — no se pudo leer el audio de ninguno de los dos lados. Lo que sigue es lo que se captó, no la llamada completa.",
  ],
  ["panel.findPlaceholder", "Find in transcript", "Buscar na transcrição", "Buscar en la transcripción"],
  ["panel.findAria", "Find in transcript", "Buscar na transcrição", "Buscar en la transcripción"],
  ["panel.matchOne", "{n} match", "{n} ocorrência", "{n} coincidencia"],
  ["panel.matchOther", "{n} matches", "{n} ocorrências", "{n} coincidencias"],
  ["panel.copy", "Copy", "Copiar", "Copiar"],
  ["panel.showInFolder", "Show in folder", "Mostrar na pasta", "Mostrar en la carpeta"],
  ["panel.audioKept", "Audio kept · 2 channels", "Áudio mantido · 2 canais", "Audio conservado · 2 canales"],
  ["panel.playAria", "Play audio", "Reproduzir áudio", "Reproducir audio"],
  [
    "panel.trustLine",
    "Transcribed on this computer · audio never left it",
    "Transcrito neste computador · o áudio nunca saiu dele",
    "Transcrito en este equipo · el audio nunca salió de él",
  ],
  ["panel.trustLineShort", "Transcribed on this computer", "Transcrito neste computador", "Transcrito en este equipo"],
  ["panel.unknownCaller", "Unknown caller", "Chamador desconhecido", "Interlocutor desconocido"],
  ["panel.otherPendingTitle", "Other calls waiting to save", "Outras chamadas aguardando para salvar", "Otras llamadas esperando para guardarse"],
  ["panel.retryNow", "Retry now", "Tentar novamente", "Reintentar ahora"],
  ["panel.cantSaveNow", "Can't save this transcript right now.", "Não é possível salvar esta transcrição agora.", "No se puede guardar esta transcripción ahora."],
  [
    "panel.safeOnComputer",
    "The transcript is safe on this computer. It moves over on its own once this is fixed.",
    "A transcrição está segura neste computador. Ela será movida automaticamente assim que isso for corrigido.",
    "La transcripción está a salvo en este equipo. Se moverá por sí sola en cuanto esto se resuelva.",
  ],
  ["panel.showLocalCopy", "Show local copy", "Mostrar cópia local", "Mostrar copia local"],
  ["panel.waitingToSaveTitle", "Transcripts waiting to save.", "Transcrições aguardando para salvar.", "Transcripciones esperando para guardarse."],
  [
    "panel.waitingToSaveBody",
    "These are safe on this computer and will move over once the issue is fixed.",
    "Elas estão seguras neste computador e serão movidas assim que o problema for corrigido.",
    "Están a salvo en este equipo y se moverán en cuanto se resuelva el problema.",
  ],
  ["panel.noSpeechYet", "No speech was picked up on this call yet.", "Nenhuma fala foi captada nesta chamada ainda.", "Aún no se ha captado voz en esta llamada."],
  ["panel.callBegan", "Call began · {time}", "Chamada iniciada · {time}", "Llamada iniciada · {time}"],
  ["panel.callEnded", "Call ended · {time}", "Chamada encerrada · {time}", "Llamada finalizada · {time}"],
  [
    "panel.truncatedNote",
    "Showing the most recent {n} turns while the call is live — the saved transcript will have all of it.",
    "Mostrando as {n} falas mais recentes enquanto a chamada está ao vivo — a transcrição salva terá tudo.",
    "Mostrando los {n} turnos más recientes mientras la llamada está en vivo — la transcripción guardada tendrá todo.",
  ],
  ["panel.listening", "Listening", "Ouvindo", "Escuchando"],
  ["panel.listeningAria", "Listening", "Ouvindo", "Escuchando"],
  ["panel.you", "You", "Você", "Tú"],
  ["panel.caller", "Caller", "Interlocutor", "Interlocutor"],
  [
    "panel.savedErrorFallback",
    "Couldn't save — it's safe on this computer.",
    "Não foi possível salvar — está seguro neste computador.",
    "No se pudo guardar — está a salvo en este equipo.",
  ],
];

const DICTS = { en: {}, "pt-BR": {}, es: {} };
for (const [key, en, ptBR, es] of ENTRIES) {
  DICTS.en[key] = en;
  DICTS["pt-BR"][key] = ptBR;
  DICTS.es[key] = es;
}

let activePref = "auto"; // "auto" | one of SUPPORTED_LOCALES — the stored preference
let activeLocale = "en"; // the resolved locale actually in use (auto -> detected)

/// Maps the OS/browser's reported language(s) to one of our 3 supported
/// locales. Defensive about running outside a browser (this module is
/// also imported by transcript-panel.test.js under plain node, which has
/// no `navigator`) — never throws, always returns a supported locale.
export function detectSystemLocale() {
  const langs = (typeof navigator !== "undefined" && (navigator.languages || (navigator.language ? [navigator.language] : []))) || [];
  for (const raw of langs) {
    if (!raw) continue;
    const l = String(raw).toLowerCase();
    if (l.startsWith("pt")) return "pt-BR"; // only Portuguese variant we ship (pt-BR, not pt-PT)
    if (l.startsWith("es")) return "es";
    if (l.startsWith("en")) return "en";
  }
  return "en";
}

/// `pref` is what's stored in settings: "auto" or an explicit locale.
/// Returns the resolved locale now in effect (never "auto" itself).
export function setLocale(pref) {
  activePref = pref === "auto" || SUPPORTED_LOCALES.includes(pref) ? pref : "auto";
  activeLocale = activePref === "auto" ? detectSystemLocale() : activePref;
  return activeLocale;
}

export function getLocalePref() {
  return activePref;
}

export function getLocale() {
  return activeLocale;
}

/// BCP-47 tag for Intl/Date formatting — "es-419" (neutral Latin American
/// Spanish) rather than bare "es" so number/date conventions don't default
/// to Spain's (this product's Spanish copy is neutral-Latin, per task brief).
export function localeTag() {
  if (activeLocale === "pt-BR") return "pt-BR";
  if (activeLocale === "es") return "es-419";
  return "en-US";
}

/// Resolves `key` against the active locale, falling back to `en` for any
/// key not yet translated in that locale, then to the key itself (visibly
/// broken instead of silently blank, easier to spot while extending the
/// dictionary). `vars` does simple `{name}` substitution — no plural rules
/// engine; call sites needing a plural (see panel.matchOne/matchOther)
/// pick the key themselves.
///
/// Substitution is a single pass over the ORIGINAL string (one combined
/// regex, computed from `str` before any replacement happens - not a
/// split/join per var run sequentially, 2026-07-16 4R re-review LOW
/// finding): with more than one var, a sequential loop can accidentally
/// re-substitute a token that only exists because an EARLIER var's own
/// *value* happened to contain literal `{laterVarName}` text (e.g.
/// `t("x", { a: "{b}", b: "y" })` looping a-then-b would turn the `{a}`
/// slot's literal "{b}" into "y" too, on the second iteration). No caller
/// value collides with a var name today and the result is always escaped
/// before it reaches the DOM either way, but this closes the class
/// entirely rather than merely not hitting it by luck.
export function t(key, vars) {
  const dict = DICTS[activeLocale] || DICTS.en;
  let str = dict[key];
  if (str === undefined) str = DICTS.en[key];
  if (str === undefined) return key;
  if (vars) {
    const keys = Object.keys(vars);
    if (keys.length) {
      const escaped = keys.map((k) => k.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
      const pattern = new RegExp(`\\{(${escaped.join("|")})\\}`, "g");
      str = str.replace(pattern, (_match, name) => String(vars[name]));
    }
  }
  return str;
}

/// Applies the active locale to every element under `root` carrying a
/// `data-i18n*` attribute — textContent (`data-i18n`), `placeholder`,
/// `aria-label`, or `title`. Call after `setLocale()` and again any time
/// new markup with these attributes is inserted (e.g. per-favorite rows
/// are built in JS, not marked up statically, so they call `t()` directly
/// instead — see app.js).
export function applyStaticI18n(root = document) {
  root.querySelectorAll("[data-i18n]").forEach((el) => {
    el.textContent = t(el.dataset.i18n);
  });
  root.querySelectorAll("[data-i18n-placeholder]").forEach((el) => {
    el.setAttribute("placeholder", t(el.dataset.i18nPlaceholder));
  });
  root.querySelectorAll("[data-i18n-aria-label]").forEach((el) => {
    el.setAttribute("aria-label", t(el.dataset.i18nAriaLabel));
  });
  root.querySelectorAll("[data-i18n-title]").forEach((el) => {
    el.setAttribute("title", t(el.dataset.i18nTitle));
  });
  if (typeof document !== "undefined" && root === document) {
    document.documentElement.lang = activeLocale;
  }
}
