// Layout of the SharedArrayBuffer between the producer (the stand-in for
// wined3d's CS thread) and the render worker. One batch slot: the producer
// waits for the render worker to take a batch before writing the next.

export const CTRL_BYTES = 64;
export const SLOT_BYTES = 8 << 20;
export const SHARED_BYTES = 1 << 20;
export const SAB_BYTES = CTRL_BYTES + SLOT_BYTES + SHARED_BYTES;
export const SLOT = CTRL_BYTES;
export const SHARED = CTRL_BYTES + SLOT_BYTES;

// Int32 indices into the control block.
export const PRODUCED = 0; // sequence number of the last batch written
export const CONSUMED = 1; // sequence number of the last batch taken
export const LEN = 2; // bytes in the slot
export const FLAGS = 3; // FIRST | LAST
export const FENCE = 4; // highest completed fence (render worker writes)
export const SHARED_SIZE = 5; // bytes of the shared region in use
export const SCENE = 6; // index of the scene in sceneNames() (test mode)

export const FIRST = 1; // first batch of a scene: start a fresh core
export const LAST = 2; // last batch of a scene
