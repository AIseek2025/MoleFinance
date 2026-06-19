// Wave 14 — `buffer-layout` ships no types; we only consume `blob`,
// which the wave-14 decoder uses for fixed-size byte arrays. The
// rest of the surface is reached indirectly through `@coral-xyz/borsh`,
// which already has its own type declarations.
declare module "buffer-layout" {
  /**
   * Stand-in for `buffer-layout::Layout` — every layout decodes to
   * `T` and encodes from `T`. We only call `.decode` and `.encode`
   * via the parent struct.
   */
  export interface BufferLayoutInstance<T> {
    decode(buffer: Buffer, offset?: number): T;
    encode(value: T, buffer: Buffer, offset?: number): number;
    span: number;
    property?: string;
  }

  export function blob(
    length: number,
    property: string,
  ): BufferLayoutInstance<Buffer>;

  export class Layout {}

  const _default: { blob: typeof blob };
  export default _default;
}
