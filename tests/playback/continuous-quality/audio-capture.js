/* Test-only decoded PCM capture; silent destination, no physical-output claim. */
class CQAudioCapture extends AudioWorkletProcessor {
  constructor() { super(); this.samples = new Float32Array(4096); this.used = 0; }
  process(inputs, outputs) {
    const input = inputs[0]?.[0];
    for (const channel of outputs[0] || []) channel.fill(0);
    if (input) for (let i = 0; i < input.length; i++) {
      this.samples[this.used++] = input[i];
      if (this.used === this.samples.length) {
        this.port.postMessage({ frame: currentFrame + i + 1 - this.samples.length, samples: this.samples }, [this.samples.buffer]);
        this.samples = new Float32Array(4096); this.used = 0;
      }
    }
    return true;
  }
}
registerProcessor('cq-audio-capture', CQAudioCapture);
