package dev.scrutate.lsp4ij;

import com.intellij.codeInsight.navigation.actions.GotoDeclarationAction;
import com.intellij.ide.plugins.IdeaPluginDescriptor;
import com.intellij.ide.plugins.PluginManagerCore;
import com.intellij.openapi.application.ApplicationInfo;
import com.intellij.openapi.application.ReadAction;
import com.intellij.openapi.editor.Document;
import com.intellij.openapi.editor.Editor;
import com.intellij.openapi.extensions.PluginId;
import com.intellij.openapi.fileEditor.FileDocumentManager;
import com.intellij.openapi.project.Project;
import com.intellij.openapi.util.TextRange;
import com.intellij.openapi.vfs.LocalFileSystem;
import com.intellij.openapi.vfs.VirtualFile;
import com.intellij.psi.PsiElement;
import com.intellij.psi.PsiFile;
import com.intellij.testFramework.PlatformTestUtil;
import com.intellij.testFramework.TestApplicationManager;
import com.intellij.testFramework.UsefulTestCase;
import com.intellij.testFramework.builders.EmptyModuleFixtureBuilder;
import com.intellij.testFramework.builders.ModuleFixtureBuilder;
import com.intellij.testFramework.fixtures.CodeInsightTestFixture;
import com.intellij.testFramework.fixtures.IdeaProjectTestFixture;
import com.intellij.testFramework.fixtures.IdeaTestFixtureFactory;
import com.intellij.testFramework.fixtures.TestFixtureBuilder;
import com.intellij.util.ThrowableRunnable;
import com.redhat.devtools.lsp4ij.LSPFileSupport;
import com.redhat.devtools.lsp4ij.LSPIJUtils;
import com.redhat.devtools.lsp4ij.LanguageServersRegistry;
import com.redhat.devtools.lsp4ij.features.LSPPsiElement;
import com.redhat.devtools.lsp4ij.features.navigation.LSPDefinitionParams;
import com.redhat.devtools.lsp4ij.launching.templates.LanguageServerTemplate;
import com.redhat.devtools.lsp4ij.launching.templates.LanguageServerTemplateManager;
import com.redhat.devtools.lsp4ij.server.definition.launching.UserDefinedLanguageServerDefinition;
import com.redhat.devtools.lsp4ij.templates.ServerMappingSettings;
import com.redhat.devtools.lsp4ij.usages.LocationData;
import org.eclipse.lsp4j.Location;
import org.eclipse.lsp4j.TextDocumentIdentifier;
import org.jetbrains.annotations.NotNull;

import java.io.IOException;
import java.nio.file.Path;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.UUID;
import java.util.concurrent.CompletableFuture;
import java.util.regex.Matcher;

/**
 * Imports integrations/jetbrains/lsp4ij-template as the Language Servers settings page's "Import
 * from custom template..." does, registers it with this run's binary, opens the
 * integrations/fixture.sh repository as the project and runs Go to Declaration on it.
 */
public class GoToDeclarationTest extends UsefulTestCase {
    private Path repo;
    private CodeInsightTestFixture fixture;
    private UserDefinedLanguageServerDefinition server;

    @Override
    protected void runBare(@NotNull ThrowableRunnable<Throwable> testRunnable) throws Throwable {
        // as CodeInsightFixtureTestCase does: the application starts off the EDT
        TestApplicationManager.getInstance();
        super.runBare(testRunnable);
    }

    @Override
    protected void setUp() throws Exception {
        super.setUp();
        repo = Path.of(env("SCRUTATE_FIXTURE")).toRealPath();
        TestFixtureBuilder<IdeaProjectTestFixture> project = IdeaTestFixtureFactory.getFixtureFactory().createFixtureBuilder(getName());
        fixture = IdeaTestFixtureFactory.getFixtureFactory().createCodeInsightFixture(project.getFixture());
        // the only content root: LSP4IJ sends rootUri only for a project with one base directory
        @SuppressWarnings({"unchecked", "rawtypes"})
        ModuleFixtureBuilder<?> module = project.addModule((Class) EmptyModuleFixtureBuilder.class);
        module.addContentRoot(repo.toString());
        fixture.setUp();
    }

    @Override
    protected void tearDown() throws Exception {
        try {
            if (server != null) {
                LanguageServersRegistry.getInstance().removeServerDefinition(fixture.getProject(), server);
            }
            fixture.tearDown();
        } catch (Throwable e) {
            addSuppressedException(e);
        } finally {
            fixture = null;
            super.tearDown();
        }
    }

    public void testGoToDeclarationOpensTheDefinitionInAnotherFile() {
        IdeaPluginDescriptor lsp4ij = PluginManagerCore.getPlugin(PluginId.getId("com.redhat.devtools.lsp4ij"));
        assertNotNull("LSP4IJ is not loaded", lsp4ij);
        ApplicationInfo ide = ApplicationInfo.getInstance();
        System.out.println("IDE: " + ide.getFullApplicationName() + " (" + ide.getBuild() + "); LSP4IJ " + lsp4ij.getVersion());
        assertEquals("0.21.0", lsp4ij.getVersion());

        Project project = fixture.getProject();
        VirtualFile root = LocalFileSystem.getInstance().refreshAndFindFileByNioFile(repo);
        assertNotNull(root);
        PlatformTestUtil.waitWithEventsDispatching(() -> "the project's base directories are " + LSPIJUtils.getRoots(project) + ", not " + root,
                () -> LSPIJUtils.getRoots(project).equals(Set.of(root)), 60);

        // the README's template, imported as "Import from custom template..." imports it; the
        // program is this run's binary, since the IDE's PATH has no scrutate
        VirtualFile templateDir = LocalFileSystem.getInstance().refreshAndFindFileByNioFile(Path.of("..", "lsp4ij-template").toAbsolutePath().normalize());
        assertNotNull("integrations/jetbrains/lsp4ij-template is missing", templateDir);
        LanguageServerTemplate template;
        try {
            template = LanguageServerTemplateManager.getInstance().importServerTemplate(templateDir);
        } catch (IOException e) {
            throw new AssertionError(e);
        }
        assertNotNull("LSP4IJ read no template there", template);
        assertEquals("Scrutate", template.getName());
        assertEquals("scrutate lsp", template.getProgramArgs());
        List<ServerMappingSettings> mappings = template.getAllMappings();
        // equals() leaves the patterns out, so they are compared here; a mapping with a file type
        // name would map an IDE file type that does not exist
        assertTrue("mappings: " + mappings, mappings.stream().anyMatch(m -> "python".equals(m.getLanguageId()) && List.of("*.py", "*.pyi").equals(m.getFileNamePatterns())));
        assertTrue("mappings: " + mappings, mappings.stream().allMatch(m -> m.getFileType() == null && m.getLanguage() == null && !m.getFileNamePatterns().isEmpty()));
        server = new UserDefinedLanguageServerDefinition(UUID.randomUUID().toString(), template.getName(), "",
                template.getProgramArgs().replaceFirst("^scrutate", Matcher.quoteReplacement(env("SCRUTATE_BIN"))), Map.of(), true, null, null);
        LanguageServersRegistry.getInstance().addServerDefinition(project, server, mappings);

        VirtualFile app = root.findFileByRelativePath("src/app.py");
        VirtualFile geo = root.findFileByRelativePath("src/geo.py");
        assertNotNull(app);
        assertNotNull(geo);
        fixture.configureFromExistingVirtualFile(app);
        Editor editor = fixture.getEditor();
        Document document = editor.getDocument();
        // `make_point` in line 5, `    return make_point().norm()`
        int line = document.getLineStartOffset(4);
        assertEquals("make_point", document.getText(new TextRange(line + 11, line + 21)));
        int offset = line + 13;
        editor.getCaretModel().moveToOffset(offset);

        // LSP4IJ's handler blocks its thread, here the EDT, until the answer comes; fill its cache
        // first while pumping events, so the handler below finds the answer done
        PsiFile file = fixture.getFile();
        LSPDefinitionParams params = new LSPDefinitionParams(new TextDocumentIdentifier(), LSPIJUtils.toPosition(offset, document), offset);
        CompletableFuture<List<LocationData>> answer =
                ReadAction.compute(() -> LSPFileSupport.getSupport(file).getDefinitionSupport().getDefinitions(params));
        List<LocationData> locations = PlatformTestUtil.waitForFuture(answer, 180_000);
        assertNotNull("no language server is mapped to src/app.py", locations);
        assertFalse("textDocument/definition answered no location", locations.isEmpty());
        Location first = locations.get(0).location();
        System.out.println("textDocument/definition: " + first.getUri() + " line " + first.getRange().getStart().getLine());
        assertTrue(first.getUri(), first.getUri().endsWith("/src/geo.py"));
        assertEquals(5, first.getRange().getStart().getLine());

        // what Navigate > Declaration or Usages asks the IDE for
        PsiElement[] targets = ReadAction.compute(() -> GotoDeclarationAction.findAllTargetElements(project, editor, offset));
        assertEquals("targets: " + List.of(targets), 1, targets.length);
        LSPPsiElement target = assertInstanceOf(targets[0], LSPPsiElement.class);
        VirtualFile targetFile = ReadAction.compute(() -> target.getContainingFile().getVirtualFile());
        assertEquals(geo, targetFile);
        Document geoDocument = FileDocumentManager.getInstance().getDocument(geo);
        assertNotNull(geoDocument);
        int targetLine = ReadAction.compute(() -> geoDocument.getLineNumber(target.getTextOffset())) + 1;
        System.out.println("Go to Declaration: src/geo.py:" + targetLine);
        assertEquals(6, targetLine);
    }

    private static String env(String name) {
        String value = System.getenv(name);
        assertNotNull("set " + name, value);
        return value;
    }
}
