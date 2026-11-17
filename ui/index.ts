// Интерфейс приложений на kit: тема, элементы экрана, своя полоса окна, экран входа.
// Стили — '@primus-fixer/kit/ui/styles.css', шрифты — '@primus-fixer/kit/ui/fonts'; правила — docs/design.md.
export { SCHEMES, applyTheme, type Scheme, type ThemeMode, type Tokens } from './theme';
export { store, stored } from './storage';
export { Icon, addIcons } from './icons';
export { Bar, Cut, Header, Row, Seg } from './elements';
export { default as TitleBar, customTitleBar, setTitleBarColors } from './TitleBar';
export { LOCKED, LockScreen, PinFlow, bioLabel, isWindows, lockCommands, type LockApi, type LockNames, type LockStatus, type PinMode } from './lock';
export { WifiSection, agoText, wifiCommands, type LanCode, type LanEvent, type LanNearby, type LanPeer, type LanView, type PairResult, type WifiApi, type WifiWords } from './wifi';
export { CloudSection, cloudCommands, lastText, stepText, type CloudApi, type CloudEvent, type CloudLast, type CloudLogin, type CloudProviderView, type CloudStatus, type CloudStep, type CloudWords } from './cloud';
