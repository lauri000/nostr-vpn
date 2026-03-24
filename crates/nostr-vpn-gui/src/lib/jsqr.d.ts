declare module 'jsqr' {
  type JsQrCode = {
    data: string
  }

  type JsQrOptions = {
    inversionAttempts?: 'dontInvert' | 'onlyInvert' | 'attemptBoth' | 'invertFirst'
  }

  export default function jsQR(
    data: Uint8ClampedArray,
    width: number,
    height: number,
    options?: JsQrOptions,
  ): JsQrCode | null
}
