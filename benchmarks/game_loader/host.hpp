// Isolated test host. Rendering/resource callbacks are test doubles.
#define _CRT_SECURE_NO_WARNINGS
#include <cassert>
#include <cstdarg>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cstdint>
#include <cstddef>
#include <string>
#include <vector>
#include <fstream>
#include <iostream>
#include <filesystem>
#include <unordered_set>
#include <unordered_map>
#include <memory>
#include <algorithm>
#include "zlib.h"
using uint=unsigned int;
using SexyString=std::string;
namespace fs=std::filesystem;
#define TOD_ASSERT(x) assert(x)
#define SMemR(p,d,n) do{std::memcpy(d,p,n);p=(void*)((uintptr_t)p+(n));}while(0)
void TodTrace(const char* f,...){va_list a;va_start(a,f);vfprintf(stderr,f,a);fputc('\n',stderr);va_end(a);}
void TodHesitationTrace(...){}
struct PerfTimer{void Start(){}};
using PFILE=FILE;
// Test callback boundary: game functions keep UTF-8 std::string paths, while
// native Windows file access uses UTF-16. Do not depend on the system ACP.
static FILE* harness_fopen(const char*path,const char*mode){
#ifdef _WIN32
 auto native=fs::u8path(path);std::wstring nativeMode;
 for(;*mode;++mode)nativeMode.push_back(static_cast<unsigned char>(*mode));
 return _wfopen(native.c_str(),nativeMode.c_str());
#else
 return fopen(path,mode);
#endif
}
#define p_fopen harness_fopen
#define p_fseek fseek
#define p_ftell ftell
#define p_fread fread
#define p_fclose fclose
struct Image{std::string mResolvedPath;};struct Font{};
using namespace std;
string SexyStringToString(const string&s){return s;}
string SexyStringToStringFast(const string&s){return s;}
string StringToSexyStringFast(const string&s){return s;}
struct SharedImageRef{Image* p=nullptr;operator Image*()const{return p;}};
static Font gFont;
static unordered_map<string,unique_ptr<Image>> gImageObjects;
static Image* image_for(const string&path){auto&image=gImageObjects[path];if(!image)image=make_unique<Image>(Image{path});return image.get();}
static unordered_map<string,string> gImages;
static unordered_set<string> gFonts;
static fs::path gRoot;
static string gLastResolvedImage;
static int gLoadCalls=0,gLoadFailures=0;
static bool exists_image(string p){replace(p.begin(),p.end(),'\\','/');for(auto ext:{".png",".jpg",".jpeg",".bmp",".gif"}){auto relative=p+ext;auto f=gRoot/fs::u8path(relative);if(fs::is_regular_file(f)){gLastResolvedImage=relative;return true;}}return false;}
struct ResourceManager{
 Image* LoadImage(const string&s){auto i=gImages.find(s);if(i==gImages.end())return nullptr;cout<<"IMAGE\t"<<s<<"\tmanifest="<<i->second<<'\n';return image_for(i->second);}
 Font* LoadFont(const string&s){return gFonts.contains(s)?&gFont:nullptr;}
};
struct App{ResourceManager* mResourceManager;SharedImageRef GetSharedImage(const string&s){++gLoadCalls;bool ok=exists_image(s);if(!ok)++gLoadFailures;return {ok?image_for(gLastResolvedImage):nullptr};}};
static ResourceManager gManager;static App gApp{&gManager};static App* gSexyApp=&gApp;
void TodAddImageToMap(SharedImageRef*,const string&s){cout<<"IMAGE\t"<<s<<"\tfallback="<<gLastResolvedImage<<'\n';} void TodMarkImageForSanding(Image*){}
enum DefFieldType{DT_INVALID,DT_INT,DT_FLOAT,DT_STRING,DT_ENUM,DT_VECTOR2,DT_ARRAY,DT_TRACK_FLOAT,DT_FLAGS,DT_IMAGE,DT_FONT};
struct DefSymbol{int mSymbolValue;const char*mSymbolName;};
struct DefField{const char*mFieldName;int mFieldOffset;DefFieldType mFieldType;void*mExtraData;};
struct DefMap{DefField*mMapFields;int mDefSize;void*(*mConstructorFunc)(void*);int mCompiledSize=0;};
struct DefinitionArrayDef{void*mArrayData;int mArrayCount;};
struct FloatParameterTrackNode{float mTime,mLowValue,mHighValue;int mCurveType,mDistribution;};
struct FloatParameterTrack{FloatParameterTrackNode*mNodes;int mCountNodes;};
struct CompressedDefinitionHeader{uint mCookie,mUncompressedSize;};
struct DefLoadResPath{const char* mPrefix;string mDirectory;};
static DefLoadResPath gDefLoadResPaths[4]={{"IMAGE_",""},{"IMAGE_","particles\\"},{"IMAGE_REANIM_","reanim\\"},{"IMAGE_REANIM_","images\\"}};
template<class T>struct TodListNode{T mValue;TodListNode*mNext=nullptr;};
template<class T>struct TodList{TodListNode<T>*mHead=nullptr,*mTail=nullptr;void AddTail(T x){auto*n=new TodListNode<T>{x};if(mTail)mTail->mNext=n;else mHead=n;mTail=n;}~TodList(){while(mHead){auto*n=mHead;mHead=n->mNext;delete n;}}};
struct ReanimatorTransform{float mTransX,mTransY,mSkewX,mSkewY,mScaleX,mScaleY,mFrame,mAlpha;Image*mImage;Font*mFont;const char*mText;};
struct ReanimatorTrack{const char*mName;ReanimatorTransform*mTransforms;int mTransformCount;};
struct ReanimatorDefinition{ReanimatorTrack*mTracks=nullptr;int mTrackCount=0;float mFPS=12;void*mReanimAtlas=nullptr;};
constexpr float DEFAULT_FIELD_PLACEHOLDER=-10000.f;
#define SDL_strcasecmp _stricmp
struct Reanimation{ReanimatorDefinition*mDefinition;int FindTrackIndex(const char*)const;void GetFramesForLayer(const char*,int&,int&);};
namespace AnimationRegistry{bool Frames(const Reanimation&,const char*,int&,int&){return false;}}

bool DefMapReadFromCache(void*&,DefMap*,void*);
bool DefinitionLoadXML(const string&,DefMap*,void*){return false;}
void* ReanimatorTrackConstructor(void*);void* ReanimatorDefinitionConstructor(void*);
