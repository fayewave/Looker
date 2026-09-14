using System;
using System.Collections.Generic;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.Graphics.Canvas;
using Windows.Data.Pdf;
using Windows.Storage.Streams;
using Windows.UI;

namespace Looker.Imaging;

/// <summary>
/// The pages of one PDF at one render size, rendered lazily. A document is laid out as a horizontal strip
/// (<see cref="Layout"/>) but only the pages the viewport actually draws get rasterized — a 300-page file
/// must not cost 300 bitmaps — and a bounded number stay resident; the rest come back on demand. Page 0 is
/// rendered by the decoder up front (it is the <see cref="DecodedImage.Bitmap"/>) and is never evicted.
/// <para>
/// Threading: <see cref="RequestPage"/>, <see cref="TryGetPage"/> and eviction all happen on the UI thread
/// (from the draw), renders run on the pool and only ever store into the slot under the lock, and the whole
/// set is disposed once the owning <see cref="DecodedImage"/> is released. The renderer itself is the inbox
/// Windows.Data.Pdf one: no codec pack, no Ghostscript.
/// </para>
/// </summary>
public sealed class PdfPageSet : IDisposable
{
    // Resident-page budget per set: enough pages for the visible window plus some scroll-back, capped so a
    // zoomed-in render (up to the viewport's vector cap per page) can't hold a gigabyte of textures.
    private const long ResidentBudgetBytes = 192L * 1024 * 1024;
    private const int MinResident = 2;
    private const int MaxResident = 12;

    private readonly PdfDocument _document;
    private readonly IDisposable _backing; // the stream the document reads pages from, lazily — outlives it
    private readonly CanvasDevice _device;
    private readonly int _box;
    private readonly CanvasBitmap?[] _pages;
    private readonly bool[] _pending;
    private readonly object _lock = new();
    private readonly SemaphoreSlim _renderGate = new(1, 1); // one page at a time per document
    private readonly CancellationTokenSource _cts = new();
    private bool _disposed;

    internal PdfPageSet(PdfDocument document, IDisposable backing, CanvasDevice device, PdfLayout layout, int box, CanvasBitmap firstPage)
    {
        _document = document;
        _backing = backing;
        _device = device;
        Layout = layout;
        _box = box;
        _pages = new CanvasBitmap?[layout.PageCount];
        _pending = new bool[layout.PageCount];
        _pages[0] = firstPage;

        (uint w, uint h) = DestinationSize(layout.MaxPageWidth, layout.MaxPageHeight, box);
        PageBytes = (long)w * h * 4;
        ResidentLimit = (int)Math.Clamp(ResidentBudgetBytes / Math.Max(1, PageBytes), MinResident, MaxResident);
    }

    public PdfLayout Layout { get; }
    public int PageCount => Layout.PageCount;

    /// <summary>Upper bound on one rendered page (the largest page at this set's box), for budgeting.</summary>
    public long PageBytes { get; }

    /// <summary>How many rendered pages this set keeps at once.</summary>
    public int ResidentLimit { get; }

    /// <summary>What the set can grow to; the cache budgets it at this (pessimistic) size.</summary>
    public long ByteSize => Math.Min(PageCount, ResidentLimit) * PageBytes;

    /// <summary>Called (on a pool thread) when a requested page has landed; the host invalidates its canvas.</summary>
    public Action? PageRendered { get; set; }

    /// <summary>The rendered bitmap for <paramref name="index"/>, or null while it is not resident.</summary>
    public CanvasBitmap? TryGetPage(int index)
    {
        if ((uint)index >= (uint)_pages.Length)
            return null;
        lock (_lock)
            return _pages[index];
    }

    /// <summary>The bitmap size a page of <paramref name="width"/>×<paramref name="height"/> content pixels
    /// renders to when its longer edge is scaled to <paramref name="box"/> pixels.</summary>
    public static (uint Width, uint Height) DestinationSize(double width, double height, int box)
    {
        double scale = box / Math.Max(Math.Max(width, height), 1.0);
        uint w = (uint)Math.Max(1, Math.Round(width * scale));
        uint h = (uint)Math.Max(1, Math.Round(height * scale));
        return (w, h);
    }

    /// <summary>Start rendering <paramref name="index"/> if it is neither resident nor in flight. Evicts the
    /// resident page furthest from it first when the set is at its limit. UI thread.</summary>
    public void RequestPage(int index)
    {
        if ((uint)index >= (uint)_pages.Length || _disposed)
            return;

        lock (_lock)
        {
            if (_pages[index] is not null || _pending[index])
                return;
            EvictIfFull(index);
            _pending[index] = true;
        }

        _ = RenderIntoSlotAsync(index);
    }

    /// <summary>Drop the resident page furthest from <paramref name="keepNear"/> until there is room for one
    /// more. Page 0 is the decoded image's bitmap and stays. Caller holds the lock.</summary>
    private void EvictIfFull(int keepNear)
    {
        while (true)
        {
            int resident = 0, victim = -1, victimDistance = -1;
            for (int i = 1; i < _pages.Length; i++)
            {
                if (_pages[i] is null)
                    continue;
                resident++;
                int distance = Math.Abs(i - keepNear);
                if (distance > victimDistance)
                {
                    victimDistance = distance;
                    victim = i;
                }
            }
            if (resident + 1 < ResidentLimit || victim < 0)
                return;
            _pages[victim]!.Dispose();
            _pages[victim] = null;
        }
    }

    private async Task RenderIntoSlotAsync(int index)
    {
        CanvasBitmap? bitmap = null;
        try
        {
            await _renderGate.WaitAsync(_cts.Token).ConfigureAwait(false);
            try
            {
                RectSize size = new(Layout.Pages[index].Width, Layout.Pages[index].Height);
                bitmap = await RenderPageAsync(_document, index, size.Width, size.Height, _box, _device, _cts.Token).ConfigureAwait(false);
            }
            finally
            {
                _renderGate.Release();
            }

            lock (_lock)
            {
                _pending[index] = false;
                if (_disposed || _pages[index] is not null)
                {
                    bitmap.Dispose();
                }
                else
                {
                    _pages[index] = bitmap;
                    bitmap = null;
                }
            }
            PageRendered?.Invoke();
        }
        catch (Exception)
        {
            // Cancelled (set disposed) or the page failed to render: leave the placeholder; a later request
            // may try again, which is right for a transient failure and cheap for a permanent one.
            bitmap?.Dispose();
            lock (_lock)
                _pending[index] = false;
        }
    }

    private readonly record struct RectSize(double Width, double Height);

    /// <summary>Rasterize one page on white at <paramref name="box"/> and upload it as a 96-dpi bitmap (the
    /// viewer works purely in pixels; see WicDecoder on why a bitmap must never carry another DPI).</summary>
    internal static async Task<CanvasBitmap> RenderPageAsync(PdfDocument document, int index, double width, double height, int box, CanvasDevice device, CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();
        (uint w, uint h) = DestinationSize(width, height, box);
        using PdfPage page = document.GetPage((uint)index);
        using var stream = new InMemoryRandomAccessStream();
        var options = new PdfPageRenderOptions
        {
            DestinationWidth = w,
            DestinationHeight = h,
            BackgroundColor = Color.FromArgb(255, 255, 255, 255),
        };
        await page.RenderToStreamAsync(stream, options).AsTask(ct).ConfigureAwait(false);
        ct.ThrowIfCancellationRequested();
        stream.Seek(0);
        CanvasBitmap bitmap = await CanvasBitmap.LoadAsync(device, stream, 96f).AsTask(ct).ConfigureAwait(false);
        if (ct.IsCancellationRequested)
        {
            bitmap.Dispose();
            throw new OperationCanceledException(ct);
        }
        return bitmap;
    }

    /// <summary>Open a document from a file, keeping the file open (shared, deletable) for lazy page reads.</summary>
    internal static async Task<(PdfDocument Document, IDisposable Backing)> OpenAsync(string path, CancellationToken ct)
    {
        var fs = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete, 1, useAsync: false);
        try
        {
            IRandomAccessStream ras = fs.AsRandomAccessStream();
            PdfDocument document = await PdfDocument.LoadFromStreamAsync(ras).AsTask(ct).ConfigureAwait(false);
            return (document, fs);
        }
        catch
        {
            fs.Dispose();
            throw;
        }
    }

    public void Dispose()
    {
        List<CanvasBitmap> bitmaps;
        lock (_lock)
        {
            if (_disposed)
                return;
            _disposed = true;
            bitmaps = new List<CanvasBitmap>(_pages.Length);
            for (int i = 0; i < _pages.Length; i++)
            {
                if (_pages[i] is { } b)
                    bitmaps.Add(b);
                _pages[i] = null;
            }
        }
        _cts.Cancel();
        foreach (CanvasBitmap b in bitmaps)
            b.Dispose();
        _backing.Dispose();
        _cts.Dispose();
    }
}
