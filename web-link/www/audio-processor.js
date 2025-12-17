/**
 * AudioWorklet processor for Link audio capture and playback.
 *
 * Handles:
 * - Capturing microphone audio and resampling from native rate to 8kHz
 * - Playing back audio and resampling from 8kHz to native rate
 * - Frame size: 320 samples @ 8kHz (40ms)
 *
 * Uses Kaiser-windowed sinc FIR filters for high-quality resampling.
 * Downsampling: Decimation with anti-aliasing lowpass filter
 * Upsampling: Zero-stuffing followed by interpolation lowpass filter
 */

const LINK_SAMPLE_RATE = 8000;
const LINK_FRAME_SIZE = 320;
const UPSAMPLE_RATIO = 6;  // 48000 / 8000
const TAPS_PER_PHASE = 8;  // Filter taps per phase (8 = good quality, 12-16 = broadcast)

/**
 * Modified Bessel function I0 (for Kaiser window)
 */
function bessel_i0(x) {
    let sum = 1.0;
    let term = 1.0;
    const x_half = x / 2;
    for (let k = 1; k < 25; k++) {
        term *= (x_half / k) * (x_half / k);
        sum += term;
        if (term < 1e-10 * sum) break;
    }
    return sum;
}

/**
 * Generate lowpass FIR filter coefficients for resampling.
 *
 * Used for both downsampling (decimation) and upsampling (interpolation).
 * Kaiser window with beta=5 provides ~50dB stopband attenuation.
 */
function generateResampleFilter(ratio, tapsPerPhase) {
    const totalTaps = ratio * tapsPerPhase;
    const center = (totalTaps - 1) / 2;
    const cutoff = 1.0 / ratio;  // Cutoff at new Nyquist

    // Kaiser window parameter
    const beta = 5.0;
    const i0_beta = bessel_i0(beta);

    // Generate filter coefficients
    const filter = new Float64Array(totalTaps);
    let sum = 0;

    for (let i = 0; i < totalTaps; i++) {
        const x = i - center;

        // Sinc function
        let sinc;
        if (Math.abs(x) < 1e-10) {
            sinc = 1.0;
        } else {
            const arg = Math.PI * x * cutoff;
            sinc = Math.sin(arg) / arg;
        }

        // Kaiser window
        const windowArg = 2.0 * i / (totalTaps - 1) - 1.0;
        const kaiser = bessel_i0(beta * Math.sqrt(Math.max(0, 1 - windowArg * windowArg))) / i0_beta;

        filter[i] = sinc * kaiser;
        sum += filter[i];
    }

    // Normalize
    for (let i = 0; i < totalTaps; i++) {
        filter[i] /= sum;
    }

    return filter;
}

// Pre-compute filter coefficients at module load time
const RESAMPLE_FILTER = generateResampleFilter(UPSAMPLE_RATIO, TAPS_PER_PHASE);
const RESAMPLE_FILTER_LEN = RESAMPLE_FILTER.length;

class LinkAudioProcessor extends AudioWorkletProcessor {
    constructor() {
        super();

        // Verify sample rate assumption
        this.resampleRatio = sampleRate / LINK_SAMPLE_RATE;
        if (Math.abs(this.resampleRatio - UPSAMPLE_RATIO) > 0.01) {
            console.warn(`Sample rate ${sampleRate} != expected 48000, ratio=${this.resampleRatio}`);
        }

        // Downsampling state (input/capture)
        this.inputBuffer = [];  // Accumulates input samples
        this.downsampleHistory = new Float64Array(RESAMPLE_FILTER_LEN);
        this.downsampleHistoryIdx = 0;
        this.downsamplePhase = 0;  // Counts input samples, output when phase hits ratio

        // Upsampling state (output/playback)
        this.outputBuffer = [];  // Queue of frames to play
        this.currentOutputFrame = null;  // Zero-stuffed and filtered frame ready for output
        this.outputFrameIndex = 0;  // Current position in output frame
        this.lastFrameTail = new Float64Array(TAPS_PER_PHASE);  // Last few samples from previous frame for continuity
        this.playbackStarted = false;  // True once we've received and started playing real audio

        // Diagnostics
        this.inputMin = Infinity;
        this.inputMax = -Infinity;
        this.outputMin = Infinity;
        this.outputMax = -Infinity;
        this.diagCounter = 0;

        // Communication with main thread
        this.port.onmessage = (event) => {
            if (event.data.type === 'playback') {
                // Received Int16 frame to play (as array)
                const samples = event.data.samples;
                // Track output sample range (Int16 values)
                for (let i = 0; i < samples.length; i++) {
                    if (samples[i] < this.outputMin) this.outputMin = samples[i];
                    if (samples[i] > this.outputMax) this.outputMax = samples[i];
                }
                this.outputBuffer.push(samples);
            }
        };

        // Report ready
        this.port.postMessage({ type: 'ready' });
    }

    /**
     * Process audio - called for each 128-sample block.
     */
    process(inputs, outputs) {
        const input = inputs[0];
        const output = outputs[0];

        // Process input (microphone capture)
        if (input.length > 0 && input[0].length > 0) {
            this.processInput(input[0]);
        }

        // Process output (playback)
        if (output.length > 0 && output[0].length > 0) {
            this.processOutput(output[0]);
            // Copy mono to all channels
            for (let ch = 1; ch < output.length; ch++) {
                output[ch].set(output[0]);
            }
        }

        return true;
    }

    /**
     * Process microphone input - downsample from native rate to 8kHz.
     * Uses FIR lowpass filter for anti-aliasing.
     */
    processInput(samples) {
        // Track input sample range for diagnostics
        for (let i = 0; i < samples.length; i++) {
            if (samples[i] < this.inputMin) this.inputMin = samples[i];
            if (samples[i] > this.inputMax) this.inputMax = samples[i];
        }

        // Report diagnostics every ~1 second
        this.diagCounter++;
        if (this.diagCounter >= 375) {
            this.port.postMessage({
                type: 'diag',
                sampleRate: sampleRate,
                resampleRatio: this.resampleRatio,
                inputMin: this.inputMin,
                inputMax: this.inputMax,
                outputMin: this.outputMin,
                outputMax: this.outputMax,
                outputBufferLen: this.outputBuffer.length
            });
            this.inputMin = Infinity;
            this.inputMax = -Infinity;
            this.outputMin = Infinity;
            this.outputMax = -Infinity;
            this.diagCounter = 0;
        }

        // Process each input sample through decimation filter
        for (let i = 0; i < samples.length; i++) {
            // Push sample into history buffer
            this.downsampleHistory[this.downsampleHistoryIdx] = samples[i];
            this.downsampleHistoryIdx = (this.downsampleHistoryIdx + 1) % RESAMPLE_FILTER_LEN;

            this.downsamplePhase++;

            // Output one sample every RATIO input samples
            if (this.downsamplePhase >= UPSAMPLE_RATIO) {
                this.downsamplePhase = 0;

                // Apply FIR filter (convolution)
                let sum = 0;
                for (let t = 0; t < RESAMPLE_FILTER_LEN; t++) {
                    const histIdx = (this.downsampleHistoryIdx + t) % RESAMPLE_FILTER_LEN;
                    sum += this.downsampleHistory[histIdx] * RESAMPLE_FILTER[t];
                }

                // Convert float to Int16 and add to buffer
                const sample16 = Math.max(-32768, Math.min(32767, Math.round(sum * 32768)));
                this.inputBuffer.push(sample16);
            }
        }

        // Send complete frames
        while (this.inputBuffer.length >= LINK_FRAME_SIZE) {
            const frame = new Int16Array(LINK_FRAME_SIZE);
            for (let i = 0; i < LINK_FRAME_SIZE; i++) {
                frame[i] = this.inputBuffer[i];
            }
            this.inputBuffer.splice(0, LINK_FRAME_SIZE);

            this.port.postMessage({
                type: 'capture',
                samples: Array.from(frame)
            });
        }
    }

    /**
     * Upsample a frame from 8kHz to 48kHz using zero-stuffing and FIR filtering.
     * Returns a Float64Array of upsampled values.
     * Uses samples from previous frame for continuity at boundaries.
     */
    upsampleFrame(inputFrame, isFirstFrame) {
        const overlapSamples = TAPS_PER_PHASE;  // Number of samples to use from previous frame
        const totalInputLen = overlapSamples + LINK_FRAME_SIZE;
        const totalOutputLen = totalInputLen * UPSAMPLE_RATIO;
        const outputLen = LINK_FRAME_SIZE * UPSAMPLE_RATIO;
        const halfLen = Math.floor(RESAMPLE_FILTER_LEN / 2);

        // For the first frame, prime lastFrameTail with the beginning of this frame
        // to avoid filter transients from all-zero history
        if (isFirstFrame) {
            for (let i = 0; i < overlapSamples; i++) {
                this.lastFrameTail[i] = inputFrame[i];
            }
        }

        // Create zero-stuffed signal with overlap from previous frame
        const stuffed = new Float64Array(totalOutputLen);

        // Add previous frame's tail samples
        for (let i = 0; i < overlapSamples; i++) {
            stuffed[i * UPSAMPLE_RATIO] = this.lastFrameTail[i] * UPSAMPLE_RATIO;
        }

        // Add current frame samples
        for (let i = 0; i < LINK_FRAME_SIZE; i++) {
            stuffed[(overlapSamples + i) * UPSAMPLE_RATIO] = inputFrame[i] * UPSAMPLE_RATIO;
        }

        // Save tail of current frame for next time
        for (let i = 0; i < overlapSamples; i++) {
            this.lastFrameTail[i] = inputFrame[LINK_FRAME_SIZE - overlapSamples + i];
        }

        // Apply lowpass filter, but only output the non-overlap portion
        const result = new Float64Array(outputLen);
        const outputOffset = overlapSamples * UPSAMPLE_RATIO;

        // Fade-in length for first frame (about 20ms at 48kHz)
        const fadeInLen = isFirstFrame ? Math.min(960, outputLen) : 0;

        for (let i = 0; i < outputLen; i++) {
            let sample = 0;
            const stuffedIdx = outputOffset + i;
            for (let t = 0; t < RESAMPLE_FILTER_LEN; t++) {
                const inputIdx = stuffedIdx - halfLen + t;
                if (inputIdx >= 0 && inputIdx < totalOutputLen) {
                    sample += stuffed[inputIdx] * RESAMPLE_FILTER[t];
                }
            }
            // Convert from Int16 scale to float
            result[i] = sample / 32768.0;

            // Apply fade-in on first frame
            if (i < fadeInLen) {
                result[i] *= i / fadeInLen;
            }
        }

        return result;
    }

    /**
     * Process output - upsample from 8kHz to native rate.
     * Uses zero-stuffing + FIR filter approach.
     * Delays output until first real frame arrives to avoid initial click.
     */
    processOutput(output) {
        // If playback hasn't started and we have no data, just output silence
        // This avoids any potential click from the audio graph before real data arrives
        if (!this.playbackStarted && this.outputBuffer.length === 0) {
            output.fill(0);
            return;
        }

        for (let i = 0; i < output.length; i++) {
            // Check if we need a new frame
            if (this.currentOutputFrame === null || this.outputFrameIndex >= this.currentOutputFrame.length) {
                if (this.outputBuffer.length > 0) {
                    const rawFrame = this.outputBuffer.shift();
                    const isFirstFrame = !this.playbackStarted;
                    this.playbackStarted = true;
                    this.currentOutputFrame = this.upsampleFrame(rawFrame, isFirstFrame);
                    this.outputFrameIndex = 0;
                } else {
                    // No frames to play - output silence (buffer underrun)
                    output[i] = 0;
                    continue;
                }
            }

            output[i] = this.currentOutputFrame[this.outputFrameIndex];
            this.outputFrameIndex++;
        }
    }
}

registerProcessor('link-audio-processor', LinkAudioProcessor);
