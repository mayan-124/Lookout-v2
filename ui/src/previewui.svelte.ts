// Whether the large preview overlay is open (shared by the disk pages).
export const pv = $state({ overlay: false });

// Thumbnail requests are numbered upwards, also across a reload of the page.
let request = Date.now();
export const nextRequest = () => ++request;
