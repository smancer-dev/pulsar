// Lightweight runtime i18n. Strings live in `tr`/`en` catalogs; `t(key, vars)`
// resolves against the active language (falling back to English, then the raw
// key). The active language is a reactive `$state`, so any `t(...)` call used in
// markup re-renders automatically when it changes.
//
// First-run language = the system language if we have it, otherwise English.
// The choice is persisted to localStorage and can be toggled at runtime.

export type Lang = 'tr' | 'en';

type Dict = Record<string, string>;

const tr: Dict = {
	// chrome / shell
	'nav.home': 'Bağlan',
	'nav.devices': 'Cihazlar',
	'nav.gaming': 'Oyunlar',
	'nav.settings': 'Ayarlar',
	'chrome.close': 'Kapat',
	'chrome.minimize': 'Küçült',
	'chrome.maximize': 'Büyüt / geri al',
	'chrome.theme': 'Tema',
	'chrome.themeToggle': 'Temayı değiştir',
	'chrome.language': 'Dil',
	'chrome.languageToggle': 'Dili değiştir',
	'tab.home': 'Home',
	'tab.close': 'Sekmeyi kapat',
	'sidebar.idLabel': 'Senin kimliğin · relay’den',
	'sidebar.thisDevice': 'Bu cihaz',
	'sidebar.me': 'Sen',
	'status.connecting': 'Bağlanıyor…',
	'status.online': 'Çevrimiçi · relay’e kayıtlı',
	'status.offline': 'Çevrimdışı',
	'status.goOnline': 'Çevrimiçi ol',
	'host.local': 'Yerel host',
	'host.chatTitle': 'Sohbet',
	'host.chatPlaceholder': 'Yanıt yaz…',
	'host.chatEmpty': 'Bağlı cihazlardan gelen mesajlar burada görünür.',
	'host.clipboardRecv': '{peer} panoyu paylaştı',
	'host.clipboardCopy': 'Kopyala',
	'host.clipboardCopied': 'Kopyalandı',
	'host.fileRecv': '{peer} dosya gönderdi: {name}',
	'host.fileSaved': '“Pulsar Alınanlar” klasörüne kaydedildi',
	'host.fileFailed': '{peer} dosya gönderimi başarısız: {name}',
	'host.toastClose': 'Kapat',
	'host.you': 'Sen',

	// client password prompt
	'pw.title': 'Host şifresi',
	'pw.lead':
		'Host’ta görünen tek seferlik şifreyi gir — ya da host karşı taraftan <b>İzin Ver</b> diyerek şifresiz onaylayabilir.',
	'pw.error': 'Yanlış şifre, tekrar dene.',
	'pw.placeholder': 'örn. 7yf2-qk',
	'pw.aria': 'Host şifresi',
	'pw.cancel': 'İptal',
	'pw.checking': 'Kontrol ediliyor…',
	'pw.submit': 'Gönder',
	'flash.close': 'Kapat',

	// host activity log
	'activity.wants': '{peer} bağlanmak istiyor (izin bekleniyor)',
	'activity.connected': '{peer} bağlandı',
	'activity.left': '{peer} ayrıldı',
	'activity.rejected': '{peer} reddedildi',
	'activity.launch': '{peer} → "{detail}" başlattı',
	'activity.stream': '{peer} akış başladı · {detail}',

	// Home
	'home.title': 'Bağlan',
	'home.sub': 'Kimliğini paylaş ya da uzak bir cihaza bağlan.',
	'home.modeRemote': 'Uzaktan masaüstü',
	'home.modeGame': 'Oyun akışı',
	'home.allowThis': 'Bu cihaza izin ver',
	'home.ready': 'Hazır',
	'home.deviceId': 'Cihaz kimliği',
	'home.copy': 'Kopyala',
	'home.copyId': 'Kimliği kopyala',
	'home.otp': 'Tek seferlik şifre',
	'home.refresh': 'Yenile',
	'home.refreshPw': 'Şifreyi yenile',
	'home.help':
		'Bu kimlik relay sunucusundan atanır. Paylaşırsan başkaları cihazına bağlanabilir; bağlantı önce P2P, gerekirse relay üzerinden kurulur. Şifre her oturumdan sonra yenilenir.',
	'home.connectedHdr': 'Bu cihaza bağlananlar',
	'home.noConnected': 'Şu an bağlı cihaz yok.',
	'home.kick': 'Bağlantıyı kes',
	'home.kickLabel': 'Kes',
	'home.startGameSession': 'Oyun oturumu başlat',
	'home.connectRemote': 'Uzak cihaza bağlan',
	'home.targetAria': 'Hedef cihaz kimliği',
	'home.fetching': 'Getiriliyor…',
	'home.fetchGames': 'Host oyunlarını getir',
	'home.noHostGames': 'Host’ta yayınlanmış oyun yok (ya da host çevrimdışı).',
	'home.connect': 'Bağlan',
	'home.recents': 'Son bağlantılar',
	'home.noRecents': 'Henüz bağlantı yok. Bir kimliğe bağlandığında burada görünür.',
	'home.remoteDevice': 'Uzak Cihaz',

	// Devices
	'devices.title': 'Cihazlar',
	'devices.sub': 'Adres defterin — kaydettiğin ve bağlandığın eşler.',
	'devices.add': 'Cihaz ekle',
	'devices.name': 'Cihaz adı',
	'devices.id': 'Cihaz kimliği',
	'devices.type': 'Tür',
	'devices.cancel': 'Vazgeç',
	'devices.addBtn': 'Ekle',
	'devices.search': 'Cihaz veya ID ara…',
	'devices.searchAria': 'Cihaz ara',
	'devices.empty': 'Henüz cihaz yok',
	'devices.emptyBody':
		'Bir kimliğe bağlandığında ya da <b>Cihaz ekle</b> ile kaydettiğinde burada listelenir.',
	'devices.fav': 'Favori',
	'devices.play': 'Oyna',
	'devices.connect': 'Bağlan',
	'devices.remove': 'Kaldır',
	'devices.defaultName': 'Cihaz',
	'devices.never': 'Hiç bağlanılmadı',
	'devices.justNow': 'az önce',
	'devices.minAgo': '{n} dk önce',
	'devices.hourAgo': '{n} saat önce',
	'devices.dayAgo': '{n} gün önce',
	'filter.all': 'Tümü',
	'cat.pc': 'Bilgisayar',
	'cat.server': 'Sunucu',
	'cat.console': 'Oyun PC’si',
	'cat.consoleShort': 'Oyun',

	// Settings
	'settings.title': 'Ayarlar',
	'settings.sub': 'Görüntü, ağ ve güvenlik tercihlerini yönet.',
	'settings.tab.display': 'Görüntü',
	'settings.tab.network': 'Ağ',
	'settings.tab.security': 'Güvenlik',
	'settings.tab.general': 'Genel',
	'settings.quality': 'Varsayılan kalite',
	'settings.qualityDesc': 'Yeni oturumlar bu profille başlar.',
	'settings.qAuto': 'Oto',
	'settings.qHq': 'Kalite',
	'settings.qFast': 'Hız',
	'settings.resolution': 'Çözünürlük',
	'settings.codec': 'Video kodek',
	'settings.codecDesc': 'Akış için video sıkıştırma biçimi.',
	'codec.auto': 'Otomatik',
	'enc.auto': 'Otomatik (en iyi donanım)',
	'enc.software': 'Yazılım (CPU)',
	'settings.encoder': 'Encoder (kodlayıcı)',
	'settings.encoderDesc':
		'Donanım encoder’ı seç; "Yazılım" CPU kullanır. "Otomatik" en iyi donanımı seçer.',
	'settings.detected': 'ffmpeg’in bulduğu donanım encoder’ları: {list}',
	'settings.detectedNone': 'yok — yazılım kullanılacak',
	'settings.decoder': 'Decoder (çözücü)',
	'settings.decoderDesc':
		'Client tarafında video çözücü. "Yazılım" CPU kullanır; "Otomatik" en iyi donanımı seçer.',
	'settings.hdr': 'HDR aktar',
	'settings.hdrDesc': 'Destekleyen ekranlarda 10-bit HDR akışı.',
	'settings.connMethod': 'Bağlantı yöntemi',
	'settings.connMethodDesc':
		'Önerilen: Otomatik. Pulsar önce doğrudan P2P dener, başarısız olursa relay’e düşer.',
	'settings.modeAuto': 'Otomatik',
	'settings.modeP2p': 'Yalnız P2P',
	'settings.modeRelay': 'Yalnız relay',
	'settings.relay': 'Relay (rendezvous) sunucusu',
	'settings.relayDesc': 'Cihaz kimliğin buradan alınır ve P2P kurulamazsa trafik buradan geçer.',
	'settings.relayAria': 'Relay sunucusu adresi',
	'settings.bwlimit': 'Bant genişliği sınırı',
	'settings.bwlimitDesc': 'Yükleme hızını kısıtla (oyun için kapalı tut).',
	'settings.saved': '✓ kaydedildi',
	'settings.unattended': 'Gözetimsiz erişim',
	'settings.unattendedDesc': 'Kalıcı şifre ile, onay olmadan bağlanmaya izin ver.',
	'settings.twofa': 'İki adımlı doğrulama',
	'settings.twofaDesc': 'Gelen bağlantılar için ek onay kodu iste.',
	'settings.record': 'Oturum kaydı',
	'settings.recordDesc': 'Uzaktan erişim oturumlarını yerel diske kaydet.',
	'settings.startup': 'Açılışta başlat',
	'settings.startupDesc': 'Pulsar sistem açıldığında otomatik çalışsın.',
	'settings.tray': 'Sistem tepsisinde çalış',
	'settings.trayDesc': 'Kapatınca tepside arka planda kalsın.',
	'settings.debug': 'Geliştirici günlüğü',
	'settings.debugDesc': 'Ana sayfada bağlantı etkinlik kayıtlarını göster (hata ayıklama).',
	'settings.version': 'Sürüm',
	'settings.versionDesc': 'GPLv3 lisansı · açık kaynak.',

	// Games
	'games.title': 'Oyunlar',
	'games.sub': 'Host’tan akıtılacak oyunlar/programlar ve genel host ayarları.',
	'games.add': 'Oyun ekle',
	'games.hostSettings': 'Genel host ayarları',
	'games.resolution': 'Çözünürlük',
	'games.fps': 'Kare hızı · {fps} fps',
	'games.fpsAria': 'Kare hızı',
	'games.bitrate': 'Bit hızı · {n} Mbps',
	'games.bitrateAria': 'Bit hızı',
	'games.empty': 'Henüz oyun yok',
	'games.emptyBody': 'Bir oyun/program <b>ekle</b> ya da aşağıdan bir klasör tarat.',
	'games.launch': 'Başlat',
	'games.edit': 'Düzenle',
	'games.remove': 'Kaldır',
	'games.folderScan': 'Klasör tarama',
	'games.folderPlaceholder': '/yol/oyun-klasörü',
	'games.folderAria': 'Klasör yolu',
	'games.addFolder': 'Klasör ekle',
	'games.stop': 'Durdur',
	'games.scanAll': 'Tümünü tara',
	'games.autoScan': 'Otomatik tarama',
	'games.every': 'her',
	'games.minutes': 'dk',
	'games.intervalAria': 'Tarama aralığı (dk)',
	'games.found': 'Bulunanlar',
	'games.addAll': 'Tümünü ekle',
	'games.addOne': 'Ekle',
	'games.editTitle': 'Oyunu düzenle',
	'games.fTitle': 'Başlık',
	'games.fTitlePlaceholder': 'Oyun / program adı',
	'games.fType': 'Tür',
	'games.fExePath': 'Çalıştırılabilir yol',
	'games.fExePlaceholder': '/yol/oyun.exe',
	'games.fArgs': 'Argümanlar (opsiyonel)',
	'games.fArgsAria': 'Argümanlar',
	'games.fCommand': 'Komut',
	'games.fCover': 'Kapak görseli (opsiyonel)',
	'games.fCoverPlaceholder': '/yol/kapak.png veya https://…',
	'games.fCmdStart': 'Oturum başlarken (komut)',
	'games.fCmdStartPlaceholder': 'ör. çözünürlük ayarla',
	'games.fCmdStartAria': 'Başlangıç komutu',
	'games.fCmdStop': 'Oturum biterken (komut)',
	'games.fCmdStopPlaceholder': 'ör. eski ayarı geri yükle',
	'games.fCmdStopAria': 'Bitiş komutu',
	'games.fSave': 'Kaydet',
	'games.fPathAria': 'Yol',
	'games.scanNeedFolder': 'Önce taranacak klasör ekle.',
	'games.scanningAuto': 'Otomatik taranıyor…',
	'games.scanning': 'Taranıyor…',
	'games.scanStopped': 'Tarama durduruldu.',
	'games.scanAutoAdded': '{n} oyun otomatik eklendi.',
	'games.scanFound': '{n} yeni uygulama bulundu.',
	'type.program': 'Program',
	'type.command': 'Komut',
	'type.image': 'Görsel',

	// Connecting
	'connecting.step1': 'Relay sunucusuna bağlanılıyor…',
	'connecting.step2': 'Eş bulunuyor (rendezvous)…',
	'connecting.step3': 'Uçtan uca şifreli oturum kuruluyor…',
	'connecting.step4': 'Host onayı bekleniyor…',
	'connecting.modeGame': 'Oyun akışı',
	'connecting.modeRemote': 'Uzaktan masaüstü',
	'connecting.cancel': 'Vazgeç',

	// Session
	'session.videoErr': 'Video bağlantısı kurulamadı.',
	'session.streamStopped': 'Yayın durdu — host ekran paylaşımını durdurmuş olabilir.',
	'session.waiting': 'Görüntü bekleniyor… Host’ta ekran paylaşım iznini onayla.',
	'session.controlOffSame':
		'Aynı cihazda kontrol kapalı · uzaktan kontrol için 2. bir cihaz kullan',
	'session.clickToControl': 'Kontrol etmek için ekrana tıkla',
	'session.controllingPre': 'Kontroldesin · çıkmak için ',
	'session.controllingSuf': '',
	'session.fullscreen': 'Tam ekran',
	'session.exitFullscreen': 'Tam ekrandan çık',
	'session.netTitle': 'Bağlantı: {conn} · {fps} FPS',
	'session.menu': 'Menü',
	'session.controls': 'Oturum kontrolleri',
	'session.clipboard': 'Pano gönder',
	'session.files': 'Dosya gönder',
	'session.chat': 'Sohbet',
	'session.mic': 'Mikrofon',
	'session.micOn': 'Mikrofon açık',
	'session.end': 'Oturumu bitir',
	'session.clipboardSent': 'Pano uzak cihaza gönderildi',
	'session.clipboardEmpty': 'Pano boş',
	'session.clipboardError': 'Pano okunamadı',
	'session.clipboardRecv': 'Uzak pano alındı (panona kopyalandı)',
	'session.fileSending': '{name} gönderiliyor…',
	'session.fileSent': '{name} gönderildi',
	'session.fileTooBig': 'Dosya çok büyük (en fazla 50 MB).',
	'session.chatPlaceholder': 'Mesaj yaz…',
	'session.chatEmpty': 'Henüz mesaj yok. İlk mesajı sen yaz.',
	'session.chatYou': 'Sen',
	'session.chatPeer': 'Karşı taraf',
	'session.back': 'Geri',
	'session.send': 'Gönder',

	// Approve popup
	'approve.title': 'Bağlantı isteği',
	'approve.lead': 'Bir cihaz bu bilgisayara bağlanmak istiyor.',
	'approve.deviceId': 'Cihaz kimliği',
	'approve.pwOk': 'Doğru şifre girildi',
	'approve.pwBad': 'Yanlış şifre girildi',
	'approve.pwNone': 'Şifresiz onaylayabilir veya şifre bekleyebilirsin',
	'approve.deny': 'Reddet',
	'approve.allow': 'İzin Ver',

	// Modal
	'modal.close': 'Kapat'
};

const en: Dict = {
	// chrome / shell
	'nav.home': 'Connect',
	'nav.devices': 'Devices',
	'nav.gaming': 'Games',
	'nav.settings': 'Settings',
	'chrome.close': 'Close',
	'chrome.minimize': 'Minimize',
	'chrome.maximize': 'Maximize / restore',
	'chrome.theme': 'Theme',
	'chrome.themeToggle': 'Change theme',
	'chrome.language': 'Language',
	'chrome.languageToggle': 'Change language',
	'tab.home': 'Home',
	'tab.close': 'Close tab',
	'sidebar.idLabel': 'Your ID · from relay',
	'sidebar.thisDevice': 'This device',
	'sidebar.me': 'You',
	'status.connecting': 'Connecting…',
	'status.online': 'Online · registered with relay',
	'status.offline': 'Offline',
	'status.goOnline': 'Go online',
	'host.local': 'Local host',
	'host.chatTitle': 'Chat',
	'host.chatPlaceholder': 'Type a reply…',
	'host.chatEmpty': 'Messages from connected devices show up here.',
	'host.clipboardRecv': '{peer} shared their clipboard',
	'host.clipboardCopy': 'Copy',
	'host.clipboardCopied': 'Copied',
	'host.fileRecv': '{peer} sent a file: {name}',
	'host.fileSaved': 'Saved to the “Pulsar Alınanlar” folder',
	'host.fileFailed': '{peer} file transfer failed: {name}',
	'host.toastClose': 'Close',
	'host.you': 'You',

	// client password prompt
	'pw.title': 'Host password',
	'pw.lead':
		'Enter the one-time password shown on the host — or the host can approve without a password by clicking <b>Allow</b>.',
	'pw.error': 'Wrong password, try again.',
	'pw.placeholder': 'e.g. 7yf2-qk',
	'pw.aria': 'Host password',
	'pw.cancel': 'Cancel',
	'pw.checking': 'Checking…',
	'pw.submit': 'Send',
	'flash.close': 'Close',

	// host activity log
	'activity.wants': '{peer} wants to connect (waiting for approval)',
	'activity.connected': '{peer} connected',
	'activity.left': '{peer} disconnected',
	'activity.rejected': '{peer} rejected',
	'activity.launch': '{peer} launched "{detail}"',
	'activity.stream': '{peer} stream started · {detail}',

	// Home
	'home.title': 'Connect',
	'home.sub': 'Share your ID or connect to a remote device.',
	'home.modeRemote': 'Remote desktop',
	'home.modeGame': 'Game streaming',
	'home.allowThis': 'Allow this device',
	'home.ready': 'Ready',
	'home.deviceId': 'Device ID',
	'home.copy': 'Copy',
	'home.copyId': 'Copy ID',
	'home.otp': 'One-time password',
	'home.refresh': 'Refresh',
	'home.refreshPw': 'Refresh password',
	'home.help':
		'This ID is assigned by the relay server. If you share it, others can connect to your device; the link is made over P2P first, falling back to the relay if needed. The password is renewed after every session.',
	'home.connectedHdr': 'Connected to this device',
	'home.noConnected': 'No devices connected right now.',
	'home.kick': 'Disconnect',
	'home.kickLabel': 'Kick',
	'home.startGameSession': 'Start a game session',
	'home.connectRemote': 'Connect to a remote device',
	'home.targetAria': 'Target device ID',
	'home.fetching': 'Fetching…',
	'home.fetchGames': 'Fetch host games',
	'home.noHostGames': 'No games published on the host (or the host is offline).',
	'home.connect': 'Connect',
	'home.recents': 'Recent connections',
	'home.noRecents': 'No connections yet. They appear here once you connect to an ID.',
	'home.remoteDevice': 'Remote device',

	// Devices
	'devices.title': 'Devices',
	'devices.sub': 'Your address book — saved and recently connected peers.',
	'devices.add': 'Add device',
	'devices.name': 'Device name',
	'devices.id': 'Device ID',
	'devices.type': 'Type',
	'devices.cancel': 'Cancel',
	'devices.addBtn': 'Add',
	'devices.search': 'Search device or ID…',
	'devices.searchAria': 'Search devices',
	'devices.empty': 'No devices yet',
	'devices.emptyBody':
		'Once you connect to an ID, or save one with <b>Add device</b>, it shows up here.',
	'devices.fav': 'Favorite',
	'devices.play': 'Play',
	'devices.connect': 'Connect',
	'devices.remove': 'Remove',
	'devices.defaultName': 'Device',
	'devices.never': 'Never connected',
	'devices.justNow': 'just now',
	'devices.minAgo': '{n} min ago',
	'devices.hourAgo': '{n} h ago',
	'devices.dayAgo': '{n} d ago',
	'filter.all': 'All',
	'cat.pc': 'Computer',
	'cat.server': 'Server',
	'cat.console': 'Gaming PC',
	'cat.consoleShort': 'Gaming',

	// Settings
	'settings.title': 'Settings',
	'settings.sub': 'Manage display, network and security preferences.',
	'settings.tab.display': 'Display',
	'settings.tab.network': 'Network',
	'settings.tab.security': 'Security',
	'settings.tab.general': 'General',
	'settings.quality': 'Default quality',
	'settings.qualityDesc': 'New sessions start with this profile.',
	'settings.qAuto': 'Auto',
	'settings.qHq': 'Quality',
	'settings.qFast': 'Speed',
	'settings.resolution': 'Resolution',
	'settings.codec': 'Video codec',
	'settings.codecDesc': 'Video compression format for streaming.',
	'codec.auto': 'Automatic',
	'enc.auto': 'Automatic (best hardware)',
	'enc.software': 'Software (CPU)',
	'settings.encoder': 'Encoder',
	'settings.encoderDesc':
		'Pick a hardware encoder; "Software" uses the CPU. "Automatic" picks the best hardware.',
	'settings.detected': 'Hardware encoders found by ffmpeg: {list}',
	'settings.detectedNone': 'none — software will be used',
	'settings.decoder': 'Decoder',
	'settings.decoderDesc':
		'Client-side video decoder. "Software" uses the CPU; "Automatic" picks the best hardware.',
	'settings.hdr': 'Stream HDR',
	'settings.hdrDesc': '10-bit HDR streaming on supported displays.',
	'settings.connMethod': 'Connection method',
	'settings.connMethodDesc':
		'Recommended: Automatic. Pulsar tries direct P2P first and falls back to the relay if that fails.',
	'settings.modeAuto': 'Automatic',
	'settings.modeP2p': 'P2P only',
	'settings.modeRelay': 'Relay only',
	'settings.relay': 'Relay (rendezvous) server',
	'settings.relayDesc':
		'Your device ID is issued here, and traffic flows through it when P2P can’t be established.',
	'settings.relayAria': 'Relay server address',
	'settings.bwlimit': 'Bandwidth limit',
	'settings.bwlimitDesc': 'Throttle upload speed (keep off for gaming).',
	'settings.saved': '✓ saved',
	'settings.unattended': 'Unattended access',
	'settings.unattendedDesc': 'Allow connecting with a fixed password, without approval.',
	'settings.twofa': 'Two-factor authentication',
	'settings.twofaDesc': 'Require an extra confirmation code for incoming connections.',
	'settings.record': 'Session recording',
	'settings.recordDesc': 'Save remote-access sessions to local disk.',
	'settings.startup': 'Launch at startup',
	'settings.startupDesc': 'Run Pulsar automatically when the system starts.',
	'settings.tray': 'Run in system tray',
	'settings.trayDesc': 'Stay in the tray in the background when closed.',
	'settings.debug': 'Developer log',
	'settings.debugDesc': 'Show the connection activity log on the home page (debugging).',
	'settings.version': 'Version',
	'settings.versionDesc': 'GPLv3 license · open source.',

	// Games
	'games.title': 'Games',
	'games.sub': 'Games/programs to stream from the host, plus general host settings.',
	'games.add': 'Add game',
	'games.hostSettings': 'General host settings',
	'games.resolution': 'Resolution',
	'games.fps': 'Frame rate · {fps} fps',
	'games.fpsAria': 'Frame rate',
	'games.bitrate': 'Bit rate · {n} Mbps',
	'games.bitrateAria': 'Bit rate',
	'games.empty': 'No games yet',
	'games.emptyBody': 'Add a game/program with <b>Add game</b>, or scan a folder below.',
	'games.launch': 'Launch',
	'games.edit': 'Edit',
	'games.remove': 'Remove',
	'games.folderScan': 'Folder scan',
	'games.folderPlaceholder': '/path/game-folder',
	'games.folderAria': 'Folder path',
	'games.addFolder': 'Add folder',
	'games.stop': 'Stop',
	'games.scanAll': 'Scan all',
	'games.autoScan': 'Auto scan',
	'games.every': 'every',
	'games.minutes': 'min',
	'games.intervalAria': 'Scan interval (min)',
	'games.found': 'Found',
	'games.addAll': 'Add all',
	'games.addOne': 'Add',
	'games.editTitle': 'Edit game',
	'games.fTitle': 'Title',
	'games.fTitlePlaceholder': 'Game / program name',
	'games.fType': 'Type',
	'games.fExePath': 'Executable path',
	'games.fExePlaceholder': '/path/game.exe',
	'games.fArgs': 'Arguments (optional)',
	'games.fArgsAria': 'Arguments',
	'games.fCommand': 'Command',
	'games.fCover': 'Cover image (optional)',
	'games.fCoverPlaceholder': '/path/cover.png or https://…',
	'games.fCmdStart': 'On session start (command)',
	'games.fCmdStartPlaceholder': 'e.g. set resolution',
	'games.fCmdStartAria': 'Start command',
	'games.fCmdStop': 'On session end (command)',
	'games.fCmdStopPlaceholder': 'e.g. restore previous setting',
	'games.fCmdStopAria': 'Stop command',
	'games.fSave': 'Save',
	'games.fPathAria': 'Path',
	'games.scanNeedFolder': 'Add a folder to scan first.',
	'games.scanningAuto': 'Auto scanning…',
	'games.scanning': 'Scanning…',
	'games.scanStopped': 'Scan stopped.',
	'games.scanAutoAdded': '{n} games auto-added.',
	'games.scanFound': '{n} new apps found.',
	'type.program': 'Program',
	'type.command': 'Command',
	'type.image': 'Image',

	// Connecting
	'connecting.step1': 'Connecting to the relay server…',
	'connecting.step2': 'Finding peer (rendezvous)…',
	'connecting.step3': 'Establishing end-to-end encrypted session…',
	'connecting.step4': 'Waiting for host approval…',
	'connecting.modeGame': 'Game streaming',
	'connecting.modeRemote': 'Remote desktop',
	'connecting.cancel': 'Cancel',

	// Session
	'session.videoErr': 'Could not establish the video connection.',
	'session.streamStopped': 'Stream stopped — the host may have stopped screen sharing.',
	'session.waiting': 'Waiting for video… Approve the screen-share permission on the host.',
	'session.controlOffSame':
		'Control disabled on the same device · use a second device for remote control',
	'session.clickToControl': 'Click the screen to take control',
	'session.controllingPre': 'In control · press ',
	'session.controllingSuf': ' to exit',
	'session.fullscreen': 'Fullscreen',
	'session.exitFullscreen': 'Exit fullscreen',
	'session.netTitle': 'Connection: {conn} · {fps} FPS',
	'session.menu': 'Menu',
	'session.controls': 'Session controls',
	'session.clipboard': 'Send clipboard',
	'session.files': 'Send file',
	'session.chat': 'Chat',
	'session.mic': 'Microphone',
	'session.micOn': 'Microphone on',
	'session.end': 'End session',
	'session.clipboardSent': 'Clipboard sent to the remote',
	'session.clipboardEmpty': 'Clipboard is empty',
	'session.clipboardError': 'Could not read the clipboard',
	'session.clipboardRecv': 'Remote clipboard received (copied to yours)',
	'session.fileSending': 'Sending {name}…',
	'session.fileSent': '{name} sent',
	'session.fileTooBig': 'File is too large (50 MB max).',
	'session.chatPlaceholder': 'Type a message…',
	'session.chatEmpty': 'No messages yet. Say hi first.',
	'session.chatYou': 'You',
	'session.chatPeer': 'Peer',
	'session.back': 'Back',
	'session.send': 'Send',

	// Approve popup
	'approve.title': 'Connection request',
	'approve.lead': 'A device wants to connect to this computer.',
	'approve.deviceId': 'Device ID',
	'approve.pwOk': 'Correct password entered',
	'approve.pwBad': 'Wrong password entered',
	'approve.pwNone': 'You can approve without a password, or wait for one',
	'approve.deny': 'Deny',
	'approve.allow': 'Allow',

	// Modal
	'modal.close': 'Close'
};

const catalogs: Record<Lang, Dict> = { tr, en };

/** Languages offered in the switcher (in toggle order). */
export const LANGS: { value: Lang; label: string; short: string }[] = [
	{ value: 'tr', label: 'Türkçe', short: 'TR' },
	{ value: 'en', label: 'English', short: 'EN' }
];

const KEY = 'pulsar.lang.v1';

/** System language if we ship it, otherwise English. */
function detect(): Lang {
	if (typeof navigator === 'undefined') return 'en';
	const langs = [navigator.language, ...(navigator.languages ?? [])];
	for (const l of langs) {
		if (typeof l === 'string' && l.toLowerCase().startsWith('tr')) return 'tr';
	}
	return 'en';
}

function load(): Lang {
	if (typeof localStorage !== 'undefined') {
		const s = localStorage.getItem(KEY);
		if (s === 'tr' || s === 'en') return s;
	}
	return detect();
}

// Reactive holder — reading `i18n.lang` inside `t()` makes every `t(...)` call
// used in markup re-run when the language changes.
export const i18n = $state<{ lang: Lang }>({ lang: load() });

export function setLang(l: Lang) {
	i18n.lang = l;
	if (typeof localStorage !== 'undefined') localStorage.setItem(KEY, l);
}

/** Toggle to the next language in `LANGS` (currently just tr ⇄ en). */
export function cycleLang() {
	const i = LANGS.findIndex((l) => l.value === i18n.lang);
	setLang(LANGS[(i + 1) % LANGS.length].value);
}

/** Translate `key`, interpolating `{name}` placeholders from `vars`. */
export function t(key: string, vars?: Record<string, string | number>): string {
	let s = catalogs[i18n.lang][key] ?? catalogs.en[key] ?? key;
	if (vars) {
		for (const k in vars) s = s.split(`{${k}}`).join(String(vars[k]));
	}
	return s;
}
