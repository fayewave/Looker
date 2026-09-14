using System;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.Graphics.Canvas;
using Windows.Data.Pdf;
using Windows.Foundation;

namespace Looker.Imaging;

/// <summary>
/// PDF through the inbox Windows.Data.Pdf renderer. The "image" is the whole document laid out as a
/// horizontal strip of pages (<see cref="PdfLayout"/>); the decode itself only opens the document, reads
/// every page size and renders page 0 — the rest render on demand from the <see cref="PdfPageSet"/> the
/// result carries. The request's target box is the longer edge of one *page*, not of the strip, so the
/// cache buckets mean the same thing they do for other images. Page sizes come back in 96-dpi pixels
/// (a US Letter page is 816×1056), which doubles as the "native" size for the 100% zoom.
/// </summary>
public sealed class PdfDecoder : IImageDecoder
{
    public bool CanDecode(ImageFormat format) => format == ImageFormat.Pdf;

    public async Task<DecodedImage?> DecodeAsync(DecodeRequest request, ImageFormat format, CanvasDevice device)
    {
        CancellationToken ct = request.Cancellation;
        int box = Math.Max(request.TargetWidth, request.TargetHeight);
        if (box <= 0)
            box = 1024;

        (PdfDocument document, IDisposable backing) = await PdfPageSet.OpenAsync(request.Path, ct).ConfigureAwait(false);
        IDisposable? owned = backing;
        try
        {
            int count = (int)Math.Min(document.PageCount, int.MaxValue);
            if (count <= 0)
                throw new InvalidOperationException("The PDF has no pages");

            var sizes = new (double, double)[count];
            for (int i = 0; i < count; i++)
            {
                ct.ThrowIfCancellationRequested();
                using PdfPage page = document.GetPage((uint)i);
                Size size = page.Size;
                sizes[i] = (size.Width, size.Height);
            }
            PdfLayout layout = PdfLayout.Compute(sizes);

            CanvasBitmap first = await PdfPageSet.RenderPageAsync(document, 0, layout.Pages[0].Width, layout.Pages[0].Height, box, device, ct).ConfigureAwait(false);
            var pages = new PdfPageSet(document, backing, device, layout, box, first);
            owned = null; // the set owns the backing stream now
            return new DecodedImage
            {
                Bitmap = first,
                OrientedNativeSize = new Size(layout.Width, layout.Height),
                IsFullResolution = false,
                Format = ImageFormat.Pdf,
                Pages = pages,
            };
        }
        finally
        {
            owned?.Dispose();
        }
    }
}
