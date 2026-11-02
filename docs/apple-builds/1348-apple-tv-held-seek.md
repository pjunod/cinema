# Keep the Apple TV timeline moving while a direction is held

Build: 284
Issue: #1348

Holding the remote's right or left direction on the focused timeline keeps
advancing the seek preview through the existing acceleration ladder. Releasing
stops the preview. Select commits the visible position and Back cancels it.
A held preview stops when another command arrives, focus leaves the timeline,
or Cinema becomes inactive. Ordinary short clicks still preview one step.
