func requestApplePlay() -> String { "play" }
func clickApplePlay() -> String { requestApplePlay() }
func onAppleClick(_ callback: () -> String) -> String { callback() }
func delegateApplePlay() -> String { onAppleClick(requestApplePlay) }
