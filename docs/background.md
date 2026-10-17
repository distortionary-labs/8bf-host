# Why 8bf-host exists

Over the past thirty years many image filters were sold as Photoshop plug-ins: sharpening and noise reduction tools, film emulation, G'MIC's 8bf build, and plenty of small utilities whose vendors no longer exist. Photoshop defined a C interface for these plug-ins in the early 1990s, and other editors adopted it so they could load the same binaries. Paint Shop Pro, Paint.NET (through PSFilterPdn) and GIMP (through pspi) all did.

All of those hosts live inside an image editor, though. If you want to apply a filter to a folder of scans, run one as a step in a build, or offer it to other programs as a service, you need a host that is not attached to an editor. 8bf-host is that host.

## How a filter host works

A filter plug-in is a shared library. It exports one entry point, and the host calls it repeatedly with a large C structure called the `FilterRecord`. That structure describes the image and the rectangles the filter wants to read and write. It also carries about two dozen tables of callbacks, which the filter uses to allocate memory, query properties, report progress and draw previews. A host has to build that structure byte for byte and answer every callback correctly; most of the code in this project does that.

A filter has to run in a process whose operating system and processor match its binary. That rule shapes most of the design; see [Where filters run](where-filters-run.md).
